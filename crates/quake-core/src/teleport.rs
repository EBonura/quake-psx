//! Original `trigger_teleport` gating, destination resolution, and telefrag.
//!
//! The original `teleport_touch` refuses to fire until `teleport_use` has set
//! `nextthink` ahead of the current time, so a targetnamed teleporter is shut
//! until it is fired and only stays open for 0.2 seconds afterwards. An
//! untargeted teleporter has no gate at all.

use psx_math::{cos_q12, sin_q12};
use quake_formats::{MapEntity, Vec3I16, Vec3I32};

use crate::targets::TargetEntitySource;

const CLASS_INFO_NULL: u8 = 0x14;
const CLASS_INFO_TELEPORT_DESTINATION: u8 = 0x19;

/// `trigger_teleport` spawnflag 1: only players may pass.
pub const SPAWNFLAG_TELEPORT_PLAYER_ONLY: u16 = 1;
/// `trigger_teleport` spawnflag 2: no teleport noise.
pub const SPAWNFLAG_TELEPORT_SILENT: u16 = 2;
/// `teleport_use` sets `nextthink = time + 0.2`, so one use opens the volume
/// for exactly twelve 60 Hz ticks.
pub const TELEPORT_ENABLE_TICKS: u16 = 12;
/// `info_teleport_destination` raises its own origin by 27 units at spawn.
pub const TELEPORT_DESTINATION_RISE_UNITS: i32 = 27;
/// `teleport_touch` leaves the player with `v_forward * 300`.
pub const TELEPORT_EXIT_SPEED_UNITS: i32 = 300;
/// `teledeath_touch` damage. The original passes 50000, which does not fit
/// this port's 16-bit damage word; `i16::MAX` is still an order of magnitude
/// above the toughest shareware monster, so the outcome is identical.
pub const TELEFRAG_DAMAGE: i16 = i16::MAX;
/// `spawn_tdeath` grows the arriving box by one unit on every axis.
pub const TELEFRAG_MARGIN_UNITS: i32 = 1;

/// Original `trigger_teleport` hum origin: the brush center unless SILENT.
#[optimize(size)]
#[inline(never)]
pub fn teleporter_hum_origin(mins: Vec3I32, maxs: Vec3I32, spawn_flags: u16) -> Option<Vec3I32> {
    if spawn_flags & SPAWNFLAG_TELEPORT_SILENT != 0 {
        return None;
    }
    // Overflow-free signed midpoint: common sign bits plus half the
    // differing bits. This stays wholly on the R3000A's native width.
    let midpoint = |low: i32, high: i32| (low & high) + ((low ^ high) >> 1);
    Some(Vec3I32 {
        x: midpoint(mins.x, maxs.x),
        y: midpoint(mins.y, maxs.y),
        z: midpoint(mins.z, maxs.z),
    })
}

/// Saturating 32-bit squared distance used to prioritise the reserved hum voice.
#[optimize(size)]
#[inline(never)]
pub fn teleporter_hum_distance(listener: Vec3I32, origin: Vec3I32) -> u32 {
    // 37,837² * 3 fits in u32. Clamping each world-unit delta there gives
    // exact ordering throughout Quake's authored coordinate range and a
    // saturating priority for malformed outliers, with only native multiplies.
    let axis = |origin: i32, listener: i32| (origin >> 12).abs_diff(listener >> 12).min(37_837);
    let x = axis(origin.x, listener.x);
    let y = axis(origin.y, listener.y);
    let z = axis(origin.z, listener.z);
    x * x + y * y + z * z
}

/// One resolved `info_teleport_destination`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TeleportTarget {
    pub destination_index: u16,
    /// Q20.12 arrival origin, already raised by the authored 27 units.
    pub origin: Vec3I32,
    pub angles: Vec3I16,
    /// Q20.12 exit push applied to the arriving player.
    pub exit_velocity: Vec3I32,
}

/// Per-teleporter open/shut state.
///
/// A teleporter with no `targetname` is permanently open. A targetnamed one
/// starts shut and each `EnableTeleport` action opens it for
/// [`TELEPORT_ENABLE_TICKS`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TeleportGate {
    always_open: bool,
    open_ticks: u16,
}

impl TeleportGate {
    #[optimize(size)]
    pub const fn new(target_name: u16) -> Self {
        Self {
            always_open: target_name == 0,
            open_ticks: 0,
        }
    }

    #[optimize(size)]
    pub const fn is_open(&self) -> bool {
        self.always_open || self.open_ticks != 0
    }

    /// Apply one `teleport_use`.
    #[optimize(size)]
    pub fn open(&mut self) {
        self.open_ticks = TELEPORT_ENABLE_TICKS;
    }

    #[optimize(size)]
    pub fn tick(&mut self, elapsed_ticks: u16) {
        self.open_ticks = self.open_ticks.saturating_sub(elapsed_ticks.max(1));
    }

    /// `teleport_touch`'s PLAYER_ONLY guard.
    #[optimize(size)]
    pub const fn admits(&self, spawn_flags: u16, toucher_is_player: bool) -> bool {
        if !self.is_open() {
            return false;
        }
        spawn_flags & SPAWNFLAG_TELEPORT_PLAYER_ONLY == 0 || toucher_is_player
    }
}

/// Resolve a `trigger_teleport`'s `target` to its authored destination.
///
/// `enabled` reports whether a cooked source index is still live, so a
/// `killtarget`ed destination is skipped exactly like the original `find`
/// walk skips a removed entity.
#[optimize(size)]
pub fn resolve_destination<S, F>(
    source: &S,
    teleport: MapEntity,
    mut enabled: F,
) -> Option<TeleportTarget>
where
    S: TargetEntitySource + ?Sized,
    F: FnMut(u16) -> bool,
{
    if teleport.target == 0 {
        return None;
    }
    for index in 0..source.entity_count() {
        let candidate = source.entity_at(index)?;
        if !matches!(
            candidate.class_name,
            CLASS_INFO_NULL | CLASS_INFO_TELEPORT_DESTINATION
        ) || candidate.target_name != teleport.target
            || !enabled(index as u16)
        {
            continue;
        }
        return Some(TeleportTarget {
            destination_index: index as u16,
            origin: Vec3I32 {
                x: candidate.origin.x,
                y: candidate.origin.y,
                z: candidate
                    .origin
                    .z
                    .saturating_add(TELEPORT_DESTINATION_RISE_UNITS << 12),
            },
            angles: candidate.angles,
            exit_velocity: exit_velocity(candidate.angles),
        });
    }
    None
}

/// `makevectors(t.mangle); other.velocity = v_forward * 300`.
///
/// Every authored shareware destination has zero pitch and roll, so this port
/// takes the horizontal forward vector only and keeps the whole computation in
/// Q12 without a second trigonometric axis.
#[optimize(size)]
pub fn exit_velocity(angles: Vec3I16) -> Vec3I32 {
    let yaw = angles.y as u16 & 0x0fff;
    Vec3I32 {
        x: cos_q12(yaw).saturating_mul(TELEPORT_EXIT_SPEED_UNITS),
        y: sin_q12(yaw).saturating_mul(TELEPORT_EXIT_SPEED_UNITS),
        z: 0,
    }
}

/// The second `spawn_tfog` from `teleport_touch`, 32 units in front of the
/// destination. This is the visible arrival flash for monsters released from
/// the sealed teleport closets used throughout Episode 1.
#[optimize(size)]
pub fn destination_fog_origin(target: TeleportTarget) -> Vec3I32 {
    let yaw = target.angles.y as u16 & 0x0fff;
    Vec3I32 {
        x: target
            .origin
            .x
            .saturating_add(cos_q12(yaw).saturating_mul(32)),
        y: target
            .origin
            .y
            .saturating_add(sin_q12(yaw).saturating_mul(32)),
        z: target.origin.z,
    }
}

/// `spawn_tdeath` box: the arriving entity's own size grown by one unit.
#[optimize(size)]
pub fn telefrag_bounds(origin: Vec3I32, mins: Vec3I32, maxs: Vec3I32) -> (Vec3I32, Vec3I32) {
    let margin = TELEFRAG_MARGIN_UNITS << 12;
    (
        Vec3I32 {
            x: origin.x.saturating_add(mins.x).saturating_sub(margin),
            y: origin.y.saturating_add(mins.y).saturating_sub(margin),
            z: origin.z.saturating_add(mins.z).saturating_sub(margin),
        },
        Vec3I32 {
            x: origin.x.saturating_add(maxs.x).saturating_add(margin),
            y: origin.y.saturating_add(maxs.y).saturating_add(margin),
            z: origin.z.saturating_add(maxs.z).saturating_add(margin),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASS_TRIGGER_TELEPORT: u8 = 0x52;

    #[optimize(size)]
    fn destination(target_name: u16, x: i32, yaw: i16) -> MapEntity {
        MapEntity {
            class_name: CLASS_INFO_TELEPORT_DESTINATION,
            target_name,
            angles: Vec3I16 { x: 0, y: yaw, z: 0 },
            origin: Vec3I32 {
                x: x << 12,
                y: 0,
                z: 0,
            },
            ..MapEntity::default()
        }
    }

    #[optimize(size)]
    #[test]
    fn untargeted_gate_is_always_open_and_targetnamed_one_starts_shut() {
        let free = TeleportGate::new(0);
        assert!(free.is_open());
        assert!(free.admits(0, true));

        let mut gated = TeleportGate::new(7);
        assert!(!gated.is_open());
        assert!(!gated.admits(0, true));
        gated.open();
        assert!(gated.admits(0, true));
    }

    #[optimize(size)]
    #[test]
    fn one_use_opens_the_gate_for_the_original_two_tenths_of_a_second() {
        let mut gate = TeleportGate::new(7);
        gate.open();
        gate.tick(TELEPORT_ENABLE_TICKS - 1);
        assert!(gate.is_open());
        gate.tick(1);
        assert!(!gate.is_open());
        gate.open();
        gate.tick(TELEPORT_ENABLE_TICKS + 5);
        assert!(!gate.is_open());
    }

    #[optimize(size)]
    #[test]
    fn one_gate_window_covers_two_monster_think_intervals() {
        let mut gate = TeleportGate::new(7);
        gate.open();
        gate.tick(crate::monster::MONSTER_THINK_TICKS);
        assert!(gate.admits(0, false));
        gate.tick(crate::monster::MONSTER_THINK_TICKS);
        assert!(!gate.admits(0, false));
    }

    #[optimize(size)]
    #[test]
    fn player_only_spawnflag_refuses_every_other_toucher() {
        let gate = TeleportGate::new(0);
        assert!(gate.admits(SPAWNFLAG_TELEPORT_PLAYER_ONLY, true));
        assert!(!gate.admits(SPAWNFLAG_TELEPORT_PLAYER_ONLY, false));
        assert!(gate.admits(0, false));
    }

    #[test]
    fn teleporter_hum_selects_the_nearest_non_silent_center_in_constant_space() {
        let listener = Vec3I32::default();
        let mut selected = None;
        let mut selected_distance = u32::MAX;
        // More candidates than the PSX's entire eleven-voice static pool:
        // selection remains one fixed-size accumulator and picks the 77th.
        for distance in (24..=100).rev() {
            let origin = teleporter_hum_origin(
                Vec3I32 {
                    x: (distance - 2) << 12,
                    y: -4 << 12,
                    z: 0,
                },
                Vec3I32 {
                    x: (distance + 2) << 12,
                    y: 4 << 12,
                    z: 8 << 12,
                },
                0,
            )
            .expect("audible teleporter");
            let candidate_distance = teleporter_hum_distance(listener, origin);
            if candidate_distance < selected_distance {
                selected = Some(origin);
                selected_distance = candidate_distance;
            }
        }
        // A closer SILENT teleporter never owns the ambient hum.
        let silent = teleporter_hum_origin(
            Vec3I32 {
                x: -1 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 1 << 12,
                y: 0,
                z: 0,
            },
            SPAWNFLAG_TELEPORT_SILENT,
        );
        assert_eq!(silent, None);
        assert_eq!(
            selected,
            Some(Vec3I32 {
                x: 24 << 12,
                y: 0,
                z: 4 << 12,
            })
        );
    }

    #[optimize(size)]
    #[test]
    fn destination_resolves_by_targetname_and_rises_the_authored_27_units() {
        let source = [
            MapEntity {
                class_name: CLASS_TRIGGER_TELEPORT,
                target: 5,
                ..MapEntity::default()
            },
            destination(4, 100, 0),
            destination(5, 200, 1024),
        ];
        let teleport = source[0];
        let target = resolve_destination(&source[..], teleport, |_| true).expect("destination");
        assert_eq!(target.destination_index, 2);
        assert_eq!(target.origin.x, 200 << 12);
        assert_eq!(target.origin.z, TELEPORT_DESTINATION_RISE_UNITS << 12);
        assert_eq!(target.angles.y, 1024);
        // Yaw 1024 of 4096 is +Y, so the authored exit push is +Y only.
        assert_eq!(target.exit_velocity.x, 0);
        assert_eq!(target.exit_velocity.y, 4096 * TELEPORT_EXIT_SPEED_UNITS);
        assert_eq!(target.exit_velocity.z, 0);
        assert_eq!(
            destination_fog_origin(target),
            Vec3I32 {
                x: 200 << 12,
                y: 32 << 12,
                z: TELEPORT_DESTINATION_RISE_UNITS << 12,
            }
        );
    }

    #[optimize(size)]
    #[test]
    fn a_killtargeted_destination_is_skipped_and_a_missing_target_resolves_to_none() {
        let source = [
            MapEntity {
                class_name: CLASS_TRIGGER_TELEPORT,
                target: 5,
                ..MapEntity::default()
            },
            destination(5, 200, 0),
            destination(5, 300, 0),
        ];
        let teleport = source[0];
        let target =
            resolve_destination(&source[..], teleport, |index| index != 1).expect("destination");
        assert_eq!(target.destination_index, 2);
        assert!(resolve_destination(&source[..], teleport, |_| false).is_none());

        let untargeted = MapEntity {
            class_name: CLASS_TRIGGER_TELEPORT,
            ..MapEntity::default()
        };
        assert!(resolve_destination(&source[..], untargeted, |_| true).is_none());
    }

    #[optimize(size)]
    #[test]
    fn telefrag_box_grows_the_arriving_player_by_one_unit() {
        let origin = Vec3I32 {
            x: 64 << 12,
            y: 0,
            z: 0,
        };
        let (mins, maxs) = telefrag_bounds(
            origin,
            Vec3I32 {
                x: -16 << 12,
                y: -16 << 12,
                z: -24 << 12,
            },
            Vec3I32 {
                x: 16 << 12,
                y: 16 << 12,
                z: 32 << 12,
            },
        );
        assert_eq!(mins.x, (64 - 17) << 12);
        assert_eq!(maxs.x, (64 + 17) << 12);
        assert_eq!(mins.z, -25 << 12);
        assert_eq!(maxs.z, 33 << 12);
    }
}
