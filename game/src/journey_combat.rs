//! Fighting policy shared by the normal-play journey controllers.
//!
//! A player closes on what is shooting at them, fires when the target is on
//! the crosshair and in useful range, keeps moving once close, and swings the
//! axe when the shells are gone. The policy reads actor positions the way a
//! player reads the screen; it never alters health, ammo, monsters or
//! positions, and every shot and step goes through the ordinary input path.

use psx_pad::button;

use crate::asset::ResidentMap;
use crate::entity::EntityScene;
use crate::input::InputFrame;
use crate::player::Player;

/// Farthest target the controller reacts to.
const ENGAGE_RANGE: i32 = 400;
/// Shotgun pellets spread; past this the shells are mostly wasted.
const SHOTGUN_RANGE: i32 = 280;
const AXE_RANGE: i32 = 72;
/// Closer than this the controller backs away from a shotgun target.
const KEEP_AWAY: i32 = 150;

pub fn controls(
    map: &ResidentMap,
    entities: &EntityScene,
    player: &Player,
    shells: u16,
) -> Option<InputFrame> {
    static mut CALLS: u32 = 0;
    // Single-threaded guest: a plain counter orders the strafe alternation.
    let tick = unsafe {
        CALLS = CALLS.wrapping_add(1);
        CALLS
    };
    let from = player.camera().origin;
    let target = entities
        .entities()
        .iter()
        .filter(|e| {
            e.is_monster()
                && e.visible
                && entities
                    .regression_monster_health(e.source_index)
                    .is_some_and(|hp| hp > 0)
        })
        .filter_map(|e| {
            let dx = (e.origin.x - from.x) >> 12;
            let dy = (e.origin.y - from.y) >> 12;
            let d = dx.abs().max(dy.abs());
            // clip_mins/maxs are world-space render bounds, not local
            // offsets, so the monster's origin is the aim point.
            (d < ENGAGE_RANGE && entities.regression_point_visible(map, from, e.origin))
                .then_some((d, e))
        })
        .min_by_key(|(d, _)| *d)?;
    let (d, target) = target;
    let dx = (target.origin.x - from.x) >> 12;
    let dy = (target.origin.y - from.y) >> 12;
    let dz = (target.origin.z - from.z) >> 12;
    let horizontal = psx_math::int32::isqrt_i32(dx * dx + dy * dy);
    let yaw = psx_math::atan2_q12(dy, dx) as i16;
    let pitch = psx_math::atan2_q12(-dz, horizontal) as i16;
    let delta = |a: i16, b: i16| -> i16 {
        let d = (i32::from(a as u16) - i32::from(b as u16)) & 4095;
        (if d > 2048 { d - 4096 } else { d }) as i16
    };
    let dyaw = delta(yaw, player.view_angles[1]);
    let dpitch = delta(pitch, player.view_angles[0]);
    // A body 40 units across subtends a wide angle up close, so the allowed
    // aim error grows as the target nears (650 is 4096 / 2 pi, Q12 per radian).
    let tolerance = (40 * 650 / d.max(1)).clamp(24, 320) as i16;
    let aligned = dyaw.abs() <= tolerance && dpitch.abs() <= tolerance;
    let useful = if shells > 0 { d <= SHOTGUN_RANGE } else { d <= AXE_RANGE };
    // Kite: back off from anything in reach, close on anything far, and
    // circle in between so grenades and bites mostly miss. Move only once
    // roughly facing the target, so movement never uses an old yaw.
    let side = if (tick / 24) & 1 == 0 { 127 } else { -127 };
    let melee_only = shells == 0;
    let movement = if dyaw.abs() > 96 {
        [0, 0]
    } else if melee_only {
        if d > AXE_RANGE { [127, 0] } else { [0, side] }
    } else if d < KEEP_AWAY {
        [-127, side]
    } else if d > SHOTGUN_RANGE - 20 {
        [127, 0]
    } else {
        [0, side]
    };
    Some(InputFrame {
        movement,
        look: [
            (i32::from(dyaw) * 4).clamp(-127, 127) as i16,
            (i32::from(dpitch) * 4).clamp(-127, 127) as i16,
        ],
        held: if aligned && useful { button::R2 } else { 0 },
        ..InputFrame::default()
    })
}
