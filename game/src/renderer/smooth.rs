//! Smooth presentation of alias-model poses.
//!
//! The decisions (when a pose counts as changed, how long a glide takes, how
//! two animation frames blend) live in `quake_core::pose` where host tests
//! cover them. This file is the renderer's side: it owns the tracker, picks
//! the key and clock, and hands the submitter either a frame straight from
//! the model or a blended copy in a scratch buffer. The submitter itself is
//! untouched, so the classic path is the same code it always was.
//!
//! Everything here runs per drawn model, not per vertex, and the image is
//! tight, so it is built for size: one shared core, no duplicated hot paths.

use alloc::boxed::Box;

use quake_affine::ClassicAliasProjectedVertex;
use quake_core::pose::{self, Pose, PoseTracker};
use quake_formats::{AliasModelView, Vec3I32};

use super::Renderer;
use crate::entity::RenderEntity;

/// Models tracked at once. Only drawn monsters and the view model are
/// observed, so this is the most that can be gliding on one screen; an
/// overflow evicts the longest unseen entry, whose model then draws unglided
/// for one think.
const POSE_SLOTS: usize = 8;
/// Tracker key of the first-person weapon. Entity indexes stay below this.
const VIEW_MODEL_KEY: u16 = u16::MAX - 1;
/// Models further than this from the eye (whole Quake units) glide but do not
/// blend: a limb moves a pixel or two between frames at that range, so the
/// nearer of the two frames is indistinguishable and the per-vertex blend is
/// the one cost of this feature that scales with what is on screen.
const BLEND_RANGE_UNITS: i32 = 800;

/// The tracker, allocated the first time a smooth pose is drawn so the classic
/// setting carries none of it. The blend buffer is not separate memory: it
/// borrows the top of the projected-vertex scratch (see `smooth_pose_core`).
pub(super) struct PoseState {
    tracker: PoseTracker<POSE_SLOTS>,
}

/// What to draw for one alias model this frame.
pub(super) struct SmoothPose {
    pub origin: [i32; 3],
    pub yaw: i16,
    /// The vertex bytes to project: a model frame, or the scratch blend.
    pub vertices: *const u8,
}

impl Renderer {
    /// A monster's pose this frame, glided and blended from its last think.
    #[optimize(size)]
    #[inline(never)]
    pub(super) fn smooth_entity_pose(
        &mut self,
        model: AliasModelView<'_>,
        key: u16,
        entity: &RenderEntity,
        frame: usize,
        current: &[u8],
        eye: Vec3I32,
    ) -> SmoothPose {
        let pose = Pose {
            origin: [entity.origin.x, entity.origin.y, entity.origin.z],
            yaw: entity.angles.y,
            frame: frame as u16,
            model: entity.model_id,
        };
        let dx = (entity.origin.x.wrapping_sub(eye.x) >> 12).clamp(-4096, 4096);
        let dy = (entity.origin.y.wrapping_sub(eye.y) >> 12).clamp(-4096, 4096);
        let dz = (entity.origin.z.wrapping_sub(eye.z) >> 12).clamp(-4096, 4096);
        let near = dx * dx + dy * dy + dz * dz <= BLEND_RANGE_UNITS * BLEND_RANGE_UNITS;
        self.smooth_pose_core(model, key, &pose, current, near)
    }

    /// The first-person weapon's vertex bytes this frame, blended between its
    /// 0.1 s frames. It has no world origin to glide.
    #[optimize(size)]
    #[inline(never)]
    pub(super) fn smooth_view_model_vertices(
        &mut self,
        model: AliasModelView<'_>,
        frame: usize,
        current: &[u8],
    ) -> *const u8 {
        let pose = Pose {
            origin: [0; 3],
            yaw: 0,
            frame: frame as u16,
            model: model.header().id,
        };
        self.smooth_pose_core(model, VIEW_MODEL_KEY, &pose, current, true)
            .vertices
    }

    /// Blend and glide monster and weapon poses, or step them as id does.
    pub fn set_smooth_poses(&mut self, smooth: bool) {
        self.smooth_poses = smooth;
    }

    /// Track `pose` under `key` and choose what to draw. When the glide has
    /// not finished and the animation frame differs, the two frames are
    /// blended per byte; `near` false takes the nearer frame instead.
    #[optimize(size)]
    #[inline(never)]
    fn smooth_pose_core(
        &mut self,
        model: AliasModelView<'_>,
        key: u16,
        pose: &Pose,
        current: &[u8],
        near: bool,
    ) -> SmoothPose {
        let state = self.pose_state.get_or_insert_with(|| {
            Box::new(PoseState {
                tracker: PoseTracker::new(),
            })
        });
        let shown = state.tracker.observe(key, self.pose_clock, pose);
        let mut result = SmoothPose {
            origin: shown.origin,
            yaw: shown.yaw,
            vertices: current.as_ptr(),
        };
        if shown.weight_q8 >= 256 || shown.from_frame == pose.frame {
            return result;
        }
        let Some(from) = model.frame_bytes(shown.from_frame as usize) else {
            return result;
        };
        if shown.weight_q8 == 0 || (!near && shown.weight_q8 < 128) {
            result.vertices = from.as_ptr();
        } else if near && from.len() == current.len() {
            // The blended bytes live at the top of the projected-vertex
            // buffer. The projection pass writes eight bytes per vertex from
            // the bottom and reads the vertex bytes as it goes, so the two
            // regions must not meet: a model too big for both is drawn on the
            // nearer frame instead (none of Episode 1's is).
            let bytes = from.len();
            let room =
                self.alias_projected.len() * core::mem::size_of::<ClassicAliasProjectedVertex>();
            if bytes / 3 * 8 + bytes <= room {
                // SAFETY: `bytes <= room`, so the slice lies inside the Vec's
                // allocation, and nothing else borrows it during this call.
                let out = unsafe {
                    core::slice::from_raw_parts_mut(
                        self.alias_projected
                            .as_mut_ptr()
                            .cast::<u8>()
                            .add(room - bytes),
                        bytes,
                    )
                };
                pose::blend_frames(from, current, shown.weight_q8, out);
                result.vertices = out.as_ptr();
            } else if shown.weight_q8 < 128 {
                result.vertices = from.as_ptr();
            }
        }
        result
    }
}
