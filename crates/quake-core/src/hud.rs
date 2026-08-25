//! Allocation-free status-bar values derived from persistent player state.

use crate::combat::{AmmoKind, ArmorTier, Inventory, Weapon};
use crate::survival::PowerupKind;
use quake_formats::GraphicsPictureId;

/// Values consumed by the PlayStation renderer for one gameplay frame.
///
/// Keeping this derivation in `quake-core` makes the HUD agree with the same
/// inventory that owns pickups, damage, weapon fallback, and map persistence.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HudView {
    pub health: u16,
    pub armor: u16,
    pub armor_tier: Option<ArmorTier>,
    /// Original weapon item bits: axe in bit zero, HUD slots in bits 1..=7.
    pub owned_weapons: u8,
    pub active_weapon: Weapon,
    /// Shells, nails, rockets and cells, in status-bar display order.
    pub ammo_pools: [u16; 4],
    pub ammo_kind: Option<AmmoKind>,
    pub ammo_label: &'static str,
    pub weapon_label: &'static str,
    pub uses_ammo: bool,
    pub keys: u8,
    /// Whole seconds left on each artifact, indexed by `PowerupKind::index`.
    /// Zero means the artifact is not held.
    pub powerup_seconds: [u8; 4],
    /// `cl.time <= cl.faceanimtime`: the status bar is showing a pain face.
    pub pain: bool,
    /// `serverflags`: one bit per rune held, `IT_SIGIL1` in bit zero.
    pub runes: u8,
}

/// `cl.faceanimtime`: the original holds the pain face for 0.2 seconds, which
/// is twelve ticks of this port's 60 Hz clock.
pub const PAIN_FACE_TICKS: u8 = 12;

/// The `Sbar_DamageTake` latch, kept beside the status bar it drives.
///
/// The original compares `cl.time` against an absolute deadline. A console
/// port with no wall clock in the HUD path counts the same window down in
/// ticks instead, from the per-frame damage signal the screen blend uses.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PainFaceTimer {
    ticks: u8,
}

impl PainFaceTimer {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self { ticks: 0 }
    }

    /// Restart the window; damage inside it extends rather than shortens it.
    #[optimize(size)]
    pub fn take_damage(&mut self) {
        self.ticks = PAIN_FACE_TICKS;
    }

    #[optimize(size)]
    pub fn tick(&mut self, elapsed_ticks: u16) {
        self.ticks = self
            .ticks
            .saturating_sub(elapsed_ticks.min(u8::MAX as u16) as u8);
    }

    #[optimize(size)]
    pub const fn active(self) -> bool {
        self.ticks != 0
    }
}

/// Left edge for a one-to-three-digit counter immediately left of an icon.
///
/// Quake's large digit pictures are 24 pixels wide even though their packed
/// VRAM origins advance by 12 words in 8bpp mode. Keeping this calculation in
/// texel space prevents the ammo value from drifting underneath its icon.
#[optimize(size)]
pub const fn right_aligned_counter_x(
    icon_x: i16,
    digit_width: i16,
    gap: i16,
    value: u16,
) -> i16 {
    let digits = if value >= 100 {
        3
    } else if value >= 10 {
        2
    } else {
        1
    };
    icon_x.saturating_sub(gap.saturating_add(digit_width.saturating_mul(digits)))
}

impl HudView {
    #[optimize(size)]
    pub const fn from_inventory(inventory: Inventory) -> Self {
        let (ammo_kind, ammo_label, weapon_label, uses_ammo) = match inventory.active_weapon() {
            Weapon::Axe => (None, "AXE", "AXE", false),
            Weapon::Shotgun => (
                Some(AmmoKind::Shells),
                "SHELLS",
                "SHOTGUN",
                true,
            ),
            Weapon::SuperShotgun => (
                Some(AmmoKind::Shells),
                "SHELLS",
                "S.SHOT",
                true,
            ),
            Weapon::Nailgun => (
                Some(AmmoKind::Nails),
                "NAILS",
                "NAILGUN",
                true,
            ),
            Weapon::SuperNailgun => (
                Some(AmmoKind::Nails),
                "NAILS",
                "S.NAIL",
                true,
            ),
            Weapon::GrenadeLauncher => (
                Some(AmmoKind::Rockets),
                "ROCKETS",
                "GRENADE",
                true,
            ),
            Weapon::RocketLauncher => (
                Some(AmmoKind::Rockets),
                "ROCKETS",
                "ROCKET",
                true,
            ),
            Weapon::Lightning => (
                Some(AmmoKind::Cells),
                "CELLS",
                "THUNDER",
                true,
            ),
        };
        let powerups = inventory.powerups();
        Self {
            health: if inventory.health() > 0 {
                inventory.health() as u16
            } else {
                0
            },
            armor: inventory.armor(),
            armor_tier: inventory.armor_tier(),
            owned_weapons: inventory.owned_weapons(),
            active_weapon: inventory.active_weapon(),
            ammo_pools: [
                inventory.ammo(AmmoKind::Shells),
                inventory.ammo(AmmoKind::Nails),
                inventory.ammo(AmmoKind::Rockets),
                inventory.ammo(AmmoKind::Cells),
            ],
            ammo_kind,
            ammo_label,
            weapon_label,
            uses_ammo,
            keys: inventory.keys(),
            powerup_seconds: [
                powerups.remaining_seconds(PowerupKind::Quad),
                powerups.remaining_seconds(PowerupKind::Pentagram),
                powerups.remaining_seconds(PowerupKind::Ring),
                powerups.remaining_seconds(PowerupKind::Biosuit),
            ],
            pain: false,
            runes: 0,
        }
    }

    #[optimize(size)]
    pub const fn active_ammo(self) -> u16 {
        match self.ammo_kind {
            Some(kind) => self.ammo_pools[kind.index()],
            None => 0,
        }
    }

    #[optimize(size)]
    pub const fn owns_weapon_slot(self, index: usize) -> bool {
        index < 7 && self.owned_weapons & (1u8 << (index + 1)) != 0
    }

    #[optimize(size)]
    pub const fn active_weapon_slot(self) -> Option<usize> {
        Some(match self.active_weapon {
            Weapon::Axe => return None,
            Weapon::Shotgun => 0,
            Weapon::SuperShotgun => 1,
            Weapon::Nailgun => 2,
            Weapon::SuperNailgun => 3,
            Weapon::GrenadeLauncher => 4,
            Weapon::RocketLauncher => 5,
            Weapon::Lightning => 6,
        })
    }

    /// Latch the pain face for this frame.
    #[optimize(size)]
    pub const fn with_pain(mut self, pain: bool) -> Self {
        self.pain = pain;
        self
    }

    /// Carry `serverflags` into the status bar's rune row.
    #[optimize(size)]
    pub const fn with_runes(mut self, runes: u8) -> Self {
        self.runes = runes;
        self
    }

    /// Original status-bar face, including the four artifact overrides and the
    /// pain strip `Sbar_Draw` picks while `cl.time <= cl.faceanimtime`.
    ///
    /// The artifact faces come first exactly as they do in the original: a
    /// quadded or invulnerable player never shows a pain face.
    #[optimize(size)]
    pub const fn face_picture(self) -> GraphicsPictureId {
        let quad = self.powerup_seconds[PowerupKind::Quad.index()] != 0;
        let invulnerability = self.powerup_seconds[PowerupKind::Pentagram.index()] != 0;
        let invisibility = self.powerup_seconds[PowerupKind::Ring.index()] != 0;
        if invisibility && invulnerability {
            GraphicsPictureId::FaceInvisibilityInvulnerability
        } else if quad {
            GraphicsPictureId::FaceQuad
        } else if invisibility {
            GraphicsPictureId::FaceInvisibility
        } else if invulnerability {
            GraphicsPictureId::FaceInvulnerability
        } else {
            let step = if self.health >= 100 {
                4
            } else {
                self.health / 25
            };
            if self.pain {
                match step {
                    0 => GraphicsPictureId::FacePain5,
                    1 => GraphicsPictureId::FacePain4,
                    2 => GraphicsPictureId::FacePain3,
                    3 => GraphicsPictureId::FacePain2,
                    _ => GraphicsPictureId::FacePain1,
                }
            } else {
                match step {
                    0 => GraphicsPictureId::Face5,
                    1 => GraphicsPictureId::Face4,
                    2 => GraphicsPictureId::Face3,
                    3 => GraphicsPictureId::Face2,
                    _ => GraphicsPictureId::Face1,
                }
            }
        }
    }

    /// `Sbar_Draw`: one picture per held rune, in `IT_SIGIL1..4` order.
    #[optimize(size)]
    pub const fn rune_picture(index: u8) -> GraphicsPictureId {
        match index {
            0 => GraphicsPictureId::Sigil1,
            1 => GraphicsPictureId::Sigil2,
            2 => GraphicsPictureId::Sigil3,
            _ => GraphicsPictureId::Sigil4,
        }
    }

    #[optimize(size)]
    pub const fn armor_picture(self) -> GraphicsPictureId {
        match self.armor_tier {
            Some(ArmorTier::Yellow) => GraphicsPictureId::Armor2,
            Some(ArmorTier::Red) => GraphicsPictureId::Armor3,
            Some(ArmorTier::Green) | None => GraphicsPictureId::Armor1,
        }
    }

    #[optimize(size)]
    pub const fn ammo_picture(self) -> Option<GraphicsPictureId> {
        match self.ammo_kind {
            Some(AmmoKind::Shells) => Some(GraphicsPictureId::Shells),
            Some(AmmoKind::Nails) => Some(GraphicsPictureId::Nails),
            Some(AmmoKind::Rockets) => Some(GraphicsPictureId::Rockets),
            Some(AmmoKind::Cells) => Some(GraphicsPictureId::Cells),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::{pickup_for_entity, Pickup};

    #[optimize(size)]
    #[test]
    fn hud_uses_the_persistent_inventory_as_its_only_authority() {
        let mut inventory = Inventory::new();
        assert_eq!(
            HudView::from_inventory(inventory),
            HudView {
                health: 100,
                armor: 0,
                armor_tier: None,
                owned_weapons: 0x03,
                active_weapon: Weapon::Shotgun,
                ammo_pools: [25, 0, 0, 0],
                ammo_kind: Some(AmmoKind::Shells),
                ammo_label: "SHELLS",
                weapon_label: "SHOTGUN",
                uses_ammo: true,
                keys: 0,
                powerup_seconds: [0; 4],
                pain: false,
                runes: 0,
            }
        );

        inventory.take_damage(37);
        inventory.apply_pickup(Pickup::Armor {
            tier: crate::combat::ArmorTier::Yellow,
            amount: 150,
        });
        inventory.apply_pickup(Pickup::Key { bit: 2 });
        inventory.apply_pickup(pickup_for_entity(0x56, 0).expect("rocket pickup"));
        assert_eq!(
            HudView::from_inventory(inventory),
            HudView {
                health: 63,
                armor: 150,
                armor_tier: Some(crate::combat::ArmorTier::Yellow),
                owned_weapons: 0x43,
                active_weapon: Weapon::RocketLauncher,
                ammo_pools: [25, 0, 5, 0],
                ammo_kind: Some(AmmoKind::Rockets),
                ammo_label: "ROCKETS",
                weapon_label: "ROCKET",
                uses_ammo: true,
                keys: 2,
                powerup_seconds: [0; 4],
                pain: false,
                runes: 0,
            }
        );
    }

    #[optimize(size)]
    #[test]
    fn dead_health_is_presented_as_zero() {
        let mut inventory = Inventory::new();
        inventory.take_damage(32000);
        assert_eq!(HudView::from_inventory(inventory).health, 0);
    }

    #[optimize(size)]
    #[test]
    fn graphical_status_bar_selects_the_original_face_armor_and_ammo_pictures() {
        let mut inventory = Inventory::new();
        let initial = HudView::from_inventory(inventory);
        assert_eq!(initial.face_picture(), GraphicsPictureId::Face1);
        assert_eq!(initial.armor_picture(), GraphicsPictureId::Armor1);
        assert_eq!(initial.ammo_picture(), Some(GraphicsPictureId::Shells));

        inventory.take_damage(37);
        inventory.apply_pickup(Pickup::Armor {
            tier: ArmorTier::Red,
            amount: 200,
        });
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Quad,
        });
        let powered = HudView::from_inventory(inventory);
        assert_eq!(powered.face_picture(), GraphicsPictureId::FaceQuad);
        assert_eq!(powered.armor_picture(), GraphicsPictureId::Armor3);

        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Ring,
        });
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Pentagram,
        });
        assert_eq!(
            HudView::from_inventory(inventory).face_picture(),
            GraphicsPictureId::FaceInvisibilityInvulnerability
        );
    }

    #[optimize(size)]
    #[test]
    fn taking_damage_shows_the_pain_strip_until_the_window_expires() {
        let mut inventory = Inventory::new();
        inventory.take_damage(30);
        let healthy = HudView::from_inventory(inventory);
        assert_eq!(healthy.face_picture(), GraphicsPictureId::Face3);

        let mut timer = PainFaceTimer::new();
        timer.take_damage();
        assert!(timer.active());
        assert_eq!(
            healthy.with_pain(timer.active()).face_picture(),
            GraphicsPictureId::FacePain3
        );

        timer.tick(PAIN_FACE_TICKS as u16 - 1);
        assert!(timer.active());
        timer.tick(1);
        assert!(!timer.active());
        assert_eq!(
            healthy.with_pain(timer.active()).face_picture(),
            GraphicsPictureId::Face3
        );
    }

    #[optimize(size)]
    #[test]
    fn the_pain_strip_covers_every_health_step_but_yields_to_the_artifacts() {
        let mut inventory = Inventory::new();
        let expected = [
            (100u16, GraphicsPictureId::FacePain1),
            (80, GraphicsPictureId::FacePain2),
            (55, GraphicsPictureId::FacePain3),
            (30, GraphicsPictureId::FacePain4),
            (5, GraphicsPictureId::FacePain5),
        ];
        for (health, face) in expected {
            let view = HudView {
                health,
                ..HudView::from_inventory(inventory)
            }
            .with_pain(true);
            assert_eq!(view.face_picture(), face);
        }

        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Quad,
        });
        assert_eq!(
            HudView::from_inventory(inventory)
                .with_pain(true)
                .face_picture(),
            GraphicsPictureId::FaceQuad
        );
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Pentagram,
        });
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Ring,
        });
        assert_eq!(
            HudView::from_inventory(inventory)
                .with_pain(true)
                .face_picture(),
            GraphicsPictureId::FaceInvisibilityInvulnerability
        );
    }

    #[optimize(size)]
    #[test]
    fn the_rune_row_maps_each_serverflag_bit_to_its_own_picture() {
        let view = HudView::from_inventory(Inventory::new()).with_runes(0b1010);
        assert_eq!(view.runes, 0b1010);
        assert_eq!(HudView::rune_picture(0), GraphicsPictureId::Sigil1);
        assert_eq!(HudView::rune_picture(1), GraphicsPictureId::Sigil2);
        assert_eq!(HudView::rune_picture(2), GraphicsPictureId::Sigil3);
        assert_eq!(HudView::rune_picture(3), GraphicsPictureId::Sigil4);
    }

    #[optimize(size)]
    #[test]
    fn ammo_counter_uses_texel_width_and_never_reaches_its_icon() {
        const ICON_X: i16 = 292;
        const DIGIT_WIDTH: i16 = 24;
        const GAP: i16 = 2;
        for (value, expected_x, digits) in [(1, 266, 1), (25, 242, 2), (100, 218, 3)] {
            let x = right_aligned_counter_x(ICON_X, DIGIT_WIDTH, GAP, value);
            assert_eq!(x, expected_x);
            assert_eq!(x + DIGIT_WIDTH * digits, ICON_X - GAP);
        }
    }

    #[optimize(size)]
    #[test]
    fn every_weapon_reports_its_active_label_and_ammo_pool() {
        let mut inventory = Inventory::new();
        let expected = [
            (Weapon::Shotgun, "SHOTGUN", "SHELLS", true),
            (Weapon::SuperShotgun, "S.SHOT", "SHELLS", true),
            (Weapon::Nailgun, "NAILGUN", "NAILS", true),
            (Weapon::SuperNailgun, "S.NAIL", "NAILS", true),
            (Weapon::GrenadeLauncher, "GRENADE", "ROCKETS", true),
            (Weapon::RocketLauncher, "ROCKET", "ROCKETS", true),
            (Weapon::Lightning, "THUNDER", "CELLS", true),
        ];
        let initial = HudView::from_inventory(inventory);
        assert_eq!(initial.weapon_label, expected[0].1);
        assert_eq!(initial.ammo_label, expected[0].2);
        assert_eq!(initial.uses_ammo, expected[0].3);
        for (class_name, (weapon, weapon_label, ammo_label, uses_ammo)) in
            [0x58, 0x55, 0x57, 0x53, 0x56, 0x54]
                .into_iter()
                .zip(expected[1..].iter().copied())
        {
            inventory.apply_pickup(pickup_for_entity(class_name, 0).expect("weapon pickup"));
            assert!(inventory.select(weapon));
            let view = HudView::from_inventory(inventory);
            assert_eq!(view.weapon_label, weapon_label);
            assert_eq!(view.ammo_label, ammo_label);
            assert_eq!(view.uses_ammo, uses_ammo);
        }

        assert!(inventory.select(Weapon::Axe));
        assert_eq!(
            HudView::from_inventory(inventory),
            HudView {
                health: 100,
                armor: 0,
                armor_tier: None,
                owned_weapons: 0xff,
                active_weapon: Weapon::Axe,
                ammo_pools: [30, 60, 10, 15],
                ammo_kind: None,
                ammo_label: "AXE",
                weapon_label: "AXE",
                uses_ammo: false,
                keys: 0,
                powerup_seconds: [0; 4],
                pain: false,
                runes: 0,
            }
        );
    }
}
