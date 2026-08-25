//! Rust player view over the fixed-point Quake locomotion core.

use quake_core::movement::{
    MovementEvents, MovementInput, MovementScratch, MovementStalls, MovementState,
};
use quake_core::view::ViewFeel;
use quake_formats::{MapEntity, Vec3I32};

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::{EntityScene, TeleportDestination};
use crate::input::InputFrame;
use crate::pusher::Rider;
use crate::renderer::Camera;

const EYE_HEIGHT_Q12: i32 = 22 << 12;
/// `PlayerDie` drops the view to `self.view_ofs = '0 0 -8'` so the corpse
/// watches the room from the floor.
const DEAD_EYE_HEIGHT_Q12: i32 = -(8 << 12);
const PLAYER_MINS_Q12: Vec3I32 = Vec3I32 {
    x: -16 << 12,
    y: -16 << 12,
    z: -24 << 12,
};
const PLAYER_MAXS_Q12: Vec3I32 = Vec3I32 {
    x: 16 << 12,
    y: 16 << 12,
    z: 32 << 12,
};
const MAX_PITCH: i16 = 1_012; // 89 degrees in Quake's 4096-unit turn.
const CLASS_INFO_PLAYER_START2: u8 = 0x18;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PlayerFrame {
    pub listener_changed: bool,
    pub events: MovementEvents,
    pub water_level: u8,
    pub water_type: i16,
    /// True when the locomotion motor ran. It only fails to run when the map
    /// has no collision provider at all, which is a load failure rather than a
    /// gameplay state; a running motor that cannot move the player is a
    /// different thing entirely and reports `moved == false` instead.
    pub motor_ran: bool,
    /// True when the motor ran and the origin changed.
    pub moved: bool,
    /// Collision queries the motor had to assume this frame.
    pub stalls: MovementStalls,
}

pub struct Player {
    movement: MovementState,
    movement_scratch: MovementScratch,
    dead: bool,
    view: ViewFeel,
    pub view_angles: [i16; 3],
}

impl Player {
    #[optimize(size)]
    fn update_look(&mut self, input: InputFrame, elapsed_ticks: u16) -> (i16, i16) {
        let ticks = i32::from(elapsed_ticks.clamp(1, 4));
        let yaw_delta = (i32::from(input.look[0]) * 32 / 127).saturating_mul(ticks) as i16;
        let pitch_delta = (i32::from(input.look[1]) * 24 / 127).saturating_mul(ticks) as i16;
        self.view_angles[1] = self.view_angles[1].wrapping_add(yaw_delta);
        self.view_angles[0] = self.view_angles[0]
            .saturating_add(pitch_delta)
            .clamp(-MAX_PITCH, MAX_PITCH);
        (yaw_delta, pitch_delta)
    }

    /// `SelectSpawnPoint`: returning to Start with any rune in hand uses
    /// `info_player_start2` instead of the cooked `info_player_start` the
    /// entity table always keeps at index one.
    pub fn from_start(map: &ResidentMap, runes: u8) -> Option<Self> {
        if runes != 0 {
            if let Some(spot) = map
                .entities()
                .iter()
                .find(|entity| entity.class_name == CLASS_INFO_PLAYER_START2)
            {
                return Self::from_entity(spot);
            }
        }
        Self::from_entity(map.entities().get(1)?)
    }

    fn from_entity(entity: MapEntity) -> Option<Self> {
        Some(Self {
            movement: MovementState::new(entity.origin),
            movement_scratch: MovementScratch::default(),
            dead: false,
            view: ViewFeel::new(),
            view_angles: [entity.angles.x, entity.angles.y, entity.angles.z],
        })
    }

    /// Adopt the death view. The corpse keeps its origin, so the camera stays
    /// where the player fell and only the eye height drops.
    pub fn set_dead(&mut self, dead: bool) {
        self.dead = dead;
    }

    /// The gameplay view: `cl.viewangles` and the eye height, before
    /// `V_CalcRefdef` layers punch, kick, roll and bob on it. Shots, aiming
    /// and the listener all use this one.
    pub fn camera(&self) -> Camera {
        let origin = self.movement.origin();
        let eye = if self.dead {
            DEAD_EYE_HEIGHT_Q12
        } else {
            EYE_HEIGHT_Q12
        };
        Camera {
            origin: Vec3I32 {
                x: origin.x,
                y: origin.y,
                z: origin.z.saturating_add(eye),
            },
            angles: self.view_angles,
        }
    }

    /// `r_refdef`: the gameplay camera plus this frame's view feel offsets.
    pub fn render_camera(&self) -> Camera {
        let mut camera = self.camera();
        let (pitch, roll, bob_z) = self.view.offsets();
        camera.origin.z = camera.origin.z.saturating_add(bob_z);
        camera.angles[0] = camera.angles[0].wrapping_add(pitch);
        camera.angles[2] = camera.angles[2].wrapping_add(roll);
        camera
    }

    /// The walk bob the view model shares with the camera, Q12 units.
    pub fn view_bob(&self) -> i32 {
        self.view.offsets().2
    }

    /// `self.punchangle_x` from a weapon fire.
    pub fn punch(&mut self, degrees: i32) {
        self.view.punch(degrees);
    }

    /// `V_ParseDamage` with `from` the attacker-to-eye direction (any
    /// length; zero for world damage).
    pub fn view_damage(&mut self, count: i32, from: Vec3I32) {
        let (forward, right, _) = quake_core::combat::view_basis(self.view_angles);
        let (side, front) = quake_core::view::damage_components(from, forward, right);
        self.view.damage(count, side, front);
    }

    /// Advance punch, kick, roll and bob for the frame about to render.
    pub fn tick_view(&mut self, elapsed_ticks: u16) {
        let (_, right, _) = quake_core::combat::view_basis(self.view_angles);
        self.view
            .tick(self.movement.velocity(), right, self.dead, elapsed_ticks);
    }

    pub fn bounds(&self) -> (Vec3I32, Vec3I32) {
        let origin = self.movement.origin();
        (add(origin, PLAYER_MINS_Q12), add(origin, PLAYER_MAXS_Q12))
    }

    /// Lend the player body to the brush-mover pass. Quake's pushers move
    /// whatever rests on them, so they need the origin and the ground flag,
    /// not just the box.
    pub fn rider(&self) -> Rider {
        let (mins, maxs) = self.bounds();
        Rider::new(self.movement.origin(), mins, maxs, self.movement.grounded())
    }

    /// Take back the origin a pusher carried this body to.
    ///
    /// The pusher already traced the move, so this is `SV_PushEntity`'s own
    /// `VectorCopy (trace.endpos, ent->v.origin)`: a placement, not a teleport.
    /// Velocity, water state and the ground flag all survive, because in the
    /// original the rider never stopped standing on the lift.
    pub fn carry_to(&mut self, origin: Vec3I32) {
        self.movement.set_origin(origin);
    }

    pub fn damage_origin(&self) -> Vec3I32 {
        let origin = self.movement.origin();
        Vec3I32 {
            x: origin.x,
            y: origin.y,
            z: origin.z.saturating_add(4 << 12),
        }
    }

    /// Current liquid submersion (0 dry .. 3 eyes under).
    pub const fn water_level(&self) -> u8 {
        self.movement.water_level()
    }

    pub fn origin(&self) -> Vec3I32 {
        self.movement.origin()
    }

    pub fn velocity(&self) -> Vec3I32 {
        self.movement.velocity()
    }

    /// `T_Damage`'s `targ.velocity = targ.velocity + dir * damage * 8` for a
    /// `MOVETYPE_WALK` target, in Q12 units per second.
    ///
    /// The movement core has no velocity setter yet, so this re-seeds it at
    /// the current origin with the summed velocity. The ground flag is
    /// cleared by that, which is what a push upward needs anyway (the motor
    /// re-derives it from the floor probe on the next tick when the push was
    /// flat), water state is re-sampled on the next tick, and the jump latch
    /// is dropped. Known compromise until the core grows `add_velocity`.
    /// `T_Damage` knockback on the player.
    pub fn add_velocity(&mut self, impulse: Vec3I32) {
        self.movement.add_velocity(impulse);
    }

    #[cfg(any(
        feature = "episode1-regression",
        feature = "episode1-route-regression",
        feature = "arsenal-regression"
    ))]
    pub fn teleport(&mut self, origin: Vec3I32) {
        self.movement.teleport(origin);
    }

    /// Apply a cooked `trigger_teleport` destination through the shipping
    /// gameplay path. The movement core clears stale velocity, liquid state,
    /// and the ground flag, then takes the authored `v_forward * 300` exit
    /// push; the destination's angles become the player view exactly like the
    /// original `other.fixangle = 1`.
    pub fn apply_teleport(&mut self, destination: TeleportDestination) {
        self.movement
            .teleport_with_velocity(destination.origin, destination.exit_velocity);
        self.view_angles = [
            destination.angles.x,
            destination.angles.y,
            destination.angles.z,
        ];
    }

    #[cfg(any(
        feature = "combat-regression",
        feature = "arsenal-regression",
        feature = "monster-regression",
        feature = "monsterjump-regression"
    ))]
    pub fn place_camera(&mut self, eye: Vec3I32, angles: [i16; 3]) {
        self.movement.teleport(Vec3I32 {
            x: eye.x,
            y: eye.y,
            z: eye.z.saturating_sub(EYE_HEIGHT_Q12),
        });
        self.view_angles = angles;
    }

    /// Advance view and locomotion using actual elapsed 60 Hz video ticks.
    pub fn update(
        &mut self,
        map: &ResidentMap,
        entities: &EntityScene,
        input: InputFrame,
        elapsed_ticks: u16,
    ) -> PlayerFrame {
        let (yaw_delta, _) = self.update_look(input, elapsed_ticks);

        let Some(collision) = entities.collision(map, self.origin()) else {
            return PlayerFrame {
                listener_changed: yaw_delta != 0,
                stalls: MovementStalls::from_bits(MovementStalls::NO_COLLISION),
                ..PlayerFrame::default()
            };
        };
        let leaves = map.leaves();
        let gravity = if map.map() == EpisodeMap::E1M8 {
            100
        } else {
            quake_core::movement::DEFAULT_GRAVITY
        };
        let frame = self.movement.update_ticks_with_gravity(
            &collision,
            &mut self.movement_scratch,
            MovementInput {
                forward: input.movement[0],
                strafe: input.movement[1],
                yaw: self.view_angles[1] as u16 & 0x0fff,
                pitch: self.view_angles[0] as u16 & 0x0fff,
                jump: input.jump_held(),
            },
            elapsed_ticks,
            gravity,
            |point| {
                let leaf = map.point_leaf_index(*point)?;
                Some(leaves.get(leaf)?.contents)
            },
        );

        PlayerFrame {
            listener_changed: yaw_delta != 0 || frame.moved,
            events: frame.events,
            water_level: frame.water_level,
            water_type: frame.water_type,
            motor_ran: true,
            moved: frame.moved,
            stalls: frame.stalls,
        }
    }

    /// Original `MOVETYPE_NOCLIP`: fly along the view direction without a
    /// hull trace, gravity, ground state, or liquid state. The fixed-point
    /// step is exactly the original 320-unit default speed over 60 Hz ticks.
    #[optimize(size)]
    pub fn update_noclip(&mut self, input: InputFrame, elapsed_ticks: u16) -> PlayerFrame {
        let (yaw_delta, pitch_delta) = self.update_look(input, elapsed_ticks);
        let (forward, right, _) = quake_core::combat::view_basis(self.view_angles);
        let ticks = i32::from(elapsed_ticks.clamp(1, 4));
        let step = |forward_axis: i32, right_axis: i32| {
            let wish = (forward_axis
                .saturating_mul(i32::from(input.movement[0]))
                .saturating_add(right_axis.saturating_mul(i32::from(input.movement[1]))))
                / 127;
            wish.saturating_mul(16 * ticks) / 3
        };
        let origin = self.origin();
        let next = Vec3I32 {
            x: origin.x.saturating_add(step(forward.x, right.x)),
            y: origin.y.saturating_add(step(forward.y, right.y)),
            z: origin.z.saturating_add(step(forward.z, right.z)),
        };
        let moved = next != origin;
        // Reseeding clears the walk motor's stale fall velocity, water and
        // ground caches so disabling noclip never launches the player.
        self.movement.teleport(next);
        PlayerFrame {
            listener_changed: yaw_delta != 0 || pitch_delta != 0 || moved,
            motor_ran: true,
            moved,
            ..PlayerFrame::default()
        }
    }
}

fn add(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x.saturating_add(right.x),
        y: left.y.saturating_add(right.y),
        z: left.z.saturating_add(right.z),
    }
}
