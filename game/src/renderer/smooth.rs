//! Smooth presentation of alias-model poses.
//!
//! The decisions (when a pose counts as changed, how long a glide takes, how
//! two animation frames blend) live in `quake_core::pose` where host tests
//! cover them. This file is the renderer's side: it owns the tracker, picks
//! the key and clock, and hands the submitter either a frame straight from
//! the model or a blended copy in a scratch buffer. The submitter itself is
//! untouched, so the classic path is the same code it always was.

use alloc::boxed::Box;

use quake_core::pose::{self, Pose, PoseTracker};
use quake_formats::AliasModelView;

use super::{Renderer, MAX_ALIAS_VERTICES};
use crate::asset::ResidentMap;
use crate::entity::RenderEntity;

/// Models tracked at once. Only drawn monsters and the view model are
/// observed, so this is the most that can be gliding on one screen; an
/// overflow evicts the longest unseen entry, whose model then draws unglided
/// for one think.
const POSE_SLOTS: usize = 24;
/// Tracker key of the first-person weapon. Entity indexes stay below this.
pub(super) const VIEW_MODEL_KEY: u16 = u16::MAX - 1;

/// Tracker and blend buffer, allocated the first time a smooth pose is drawn
/// so the classic setting carries none of it.
pub(super) struct PoseState {
    tracker: PoseTracker<POSE_SLOTS>,
    frame: [u8; MAX_ALIAS_VERTICES * 3],
}

/// What to draw for one alias model this frame.
pub(super) struct SmoothPose {
    pub origin: [i32; 3],
    pub angles: [i16; 3],
    /// The vertex bytes to project: a model frame, or the scratch blend.
    pub vertices: *const u8,
}

impl Renderer {
    /// Choose stepped (`false`, id's behaviour) or smooth monster and weapon
    /// poses for the next frames.
    #[inline(never)]
    pub fn set_smooth_poses(&mut self, smooth: bool) {
        self.smooth_poses = smooth;
    }

    /// Pose for the entity or view model `key` whose sim state is `origin`,
    /// `angles`, `frame` of `model_id`.
    ///
    /// `model` supplies the frame bytes. When the glide has not finished and
    /// the animation frame differs, the two frames are blended per byte into
    /// the scratch buffer; at weight zero the previous frame is used as is.
    #[inline(never)]
    pub(super) fn smooth_alias_pose(
        &mut self,
        model: AliasModelView<'_>,
        key: u16,
        origin: [i32; 3],
        angles: [i16; 3],
        model_id: i16,
        frame: usize,
        current: &[u8],
    ) -> SmoothPose {
        let state = self.pose_state.get_or_insert_with(|| {
            Box::new(PoseState {
                tracker: PoseTracker::new(),
                frame: [0; MAX_ALIAS_VERTICES * 3],
            })
        });
        let observed = Pose {
            origin,
            angles,
            frame: frame as u16,
            model: model_id,
        };
        let blend = state.tracker.observe(key, self.pose_clock, observed);
        let mut shown = SmoothPose {
            origin,
            angles,
            vertices: current.as_ptr(),
        };
        if blend.settled() {
            return shown;
        }
        let (glide_origin, glide_angles) = pose::glide(&blend.from, &observed, blend.weight_q8);
        shown.origin = glide_origin;
        shown.angles = glide_angles;
        let from_frame = blend.from.frame as usize;
        if from_frame == frame {
            return shown;
        }
        let Some(from) = model.frame_bytes(from_frame) else {
            return shown;
        };
        if blend.weight_q8 == 0 {
            shown.vertices = from.as_ptr();
        } else if from.len() <= state.frame.len() && from.len() == current.len() {
            pose::blend_frames(from, current, blend.weight_q8, &mut state.frame[..from.len()]);
            shown.vertices = state.frame.as_ptr();
        }
        shown
    }

    /// The tint an alias entity is lit with under the gliding style table:
    /// `R_LightPoint` over the entity's leaf, read now rather than at the last
    /// tenth-of-a-second boundary.
    #[inline(never)]
    pub(super) fn smooth_entity_light(&self, map: &ResidentMap, entity: &RenderEntity) -> u8 {
        match map.leaves().get(entity.leaf_index as usize) {
            Some(leaf) => quake_core::lightstyle::sample_leaf(
                leaf.lightmap,
                leaf.light_styles,
                &self.light_styles,
            ),
            None => entity.light,
        }
    }
}
