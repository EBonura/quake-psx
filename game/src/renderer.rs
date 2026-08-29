//! Quake BSP world rendering through PSoXide's classic-affine path.

use alloc::vec;
use alloc::vec::Vec;
use core::mem::MaybeUninit;
use core::ptr::{self, addr_of_mut};

#[cfg(feature = "renderer-hoisted-indexed-world")]
use psx_bsp::resident::IndexedVertices;
#[cfg(any(
    not(all(feature = "renderer-quake-baked-materialize", target_arch = "mips")),
    feature = "renderer-hoisted-indexed-world",
    feature = "renderer-indexed-projection"
))]
use psx_engine::materialize_classic_affine_indexed_baked_vertices;
#[cfg(not(feature = "renderer-quake-specialized-kernel"))]
use psx_engine::submit_classic_affine_batch;
#[cfg(any(
    feature = "renderer-fused-materialize-project",
    feature = "renderer-indexed-projection"
))]
use psx_engine::submit_classic_affine_projected_batch;
#[cfg(feature = "renderer-quake-specialized-kernel")]
use psx_engine::submit_quake_classic_affine_batch;
use psx_engine::{
    attributed_clip::{
        clip_convex_plane_uninit, lerp_q12_i32_rounded, AttributedClipPlane, ClipTraversal,
    },
    compose_classic_alias_transform, materialize_classic_affine_indexed_vertices,
    submit_classic_affine_scoped_windowed_fan, submit_classic_alias_model,
    submit_classic_alias_view_model, ClassicAffineBatchSurface, ClassicAffineIndexedCorner,
    ClassicAffinePosition, ClassicAffineProfile, ClassicAffineSubmit, ClassicAffineVertex,
    ClassicAliasFace, ClassicAliasProjectedVertex, ClassicAliasVertex,
};
#[cfg(feature = "renderer-census")]
use psx_engine::{
    census_classic_affine_projected_batch_topology,
    collect_classic_affine_projected_subdivision_requests, ClassicAffineSubdivisionRequest,
    ClassicAffineTopologyCensus,
};
#[cfg(feature = "renderer-indexed-projection")]
use psx_engine::{
    collect_classic_affine_indexed_projection_slots,
    materialize_classic_affine_indexed_baked_vertices_with_projection_slots,
    project_classic_affine_indexed_vertices_dense,
};
#[cfg(feature = "renderer-fused-materialize-project")]
use psx_engine::{
    materialize_project_classic_affine_indexed_batch, ClassicAffineIndexedBatchSource,
};
#[cfg(feature = "renderer-subdivision-cache")]
use psx_engine::{
    submit_classic_affine_cached_subdivision_batch, ClassicAffineSubdivisionCacheSink,
    ClassicAffineSubdivisionRootSlot,
};
#[cfg(feature = "renderer-topology-cache")]
use psx_engine::{
    submit_classic_affine_planned_resident_batch, ClassicAffinePacketPlan,
    ClassicAffinePlannedSubmit, ClassicAffineResidentBatchSurface,
};
use psx_gpu::material::{BlendMode, TextureMaterial, TextureWindow};
use psx_gpu::prim::{ClassicTriTextured, LineMono, QuadTextured, QuadTexturedMaterial, RectFlat};
use psx_gte::math::{Mat3I16, Vec3I16 as GteVec3I16, Vec3I32 as GteVec3I32};

#[cfg(all(
    feature = "renderer-fused-materialize-project",
    any(
        feature = "renderer-indexed-projection",
        feature = "renderer-subdivision-cache",
        feature = "renderer-topology-cache"
    )
))]
compile_error!(
    "renderer-fused-materialize-project is an alternative ordinary-world batch submitter"
);
use psx_gte::scene::{self, AabbClipPlane};
use psx_math::int32::{isqrt_i32, mul_q12_i32, mul_q12_i32_wide, square_i32_saturating};
use psx_math::{atan2_q12, cos_q12, sin_q12};
use psx_render_contract::{CookedDrawSurface, RetainedSurfaceBounds};
use quake_core::collision::{CONTENTS_EMPTY, CONTENTS_WATER};
use quake_core::combat::{view_basis, WeaponView, LIGHTNING_BOLT_MODEL_ID};
use quake_core::effects::{DynamicLight, ExplosionEffect, ImpactParticle, BUBBLE_SPRITE_MODEL_ID};
use quake_core::hud::HudView;
use quake_core::level::IntermissionView;
use quake_core::menu::{
    HudMode, MenuPage, MenuView, CONTROL_LINES, DEFAULT_HUD_MODE, OPTIONS_MUSIC_VOLUME_ROW,
    OPTIONS_SOUND_VOLUME_ROW,
};
use quake_core::view_model;
use quake_formats::{
    alias_model_is_sprite, AliasModelView, CompactPlane, Face, GraphicsPicture, GraphicsPictureId,
    Plane, TextureInfo, Vec3I32, FACE_BACKSIDE, FACE_BAKED_LIGHT, FACE_BAKED_UV,
    GRAPHICS_WEAPON_ICON_OFFSETS, GRAPHICS_WEAPON_ICON_VARIANT_BYTES, TEXTURE_INVISIBLE,
    TEXTURE_LAYERED_SKY, TEXTURE_LIQUID, TEXTURE_NULL, TEXTURE_SKY,
};

use crate::asset::{texture_rect, EpisodeMap, ResidentMap};
use crate::entity::{model_rotates, LightningBeam, RenderEntity};
use crate::platform::QuakeViewTransform;

const GPU_ARENA_BYTES: usize = 0x20000;
const GPU_ARENA_WORDS: usize = GPU_ARENA_BYTES / core::mem::size_of::<u32>();
// Closed Episode 1 corpus maximum (E1M4). The host cooker pins this bound so
// the guest can keep both the PVS mask and its ordered visible-face cache
// tightly sized instead of reserving for the PSB wire-format maximum.
const MAX_FACE_COUNT: usize = 6_614;
// Exhaustive PVS census maximum is 1,325 faces (E1M1). The host asset gate
// pins the closed Episode 1 corpus; the guest also fails closed if a future
// map exceeds this cache instead of growing the monotonic heap.
const MAX_VISIBLE_FACE_COUNT: usize = 1_325;
#[cfg(all(
    any(
        feature = "renderer-compact-cell-stream",
        feature = "renderer-cell-policy"
    ),
    not(feature = "renderer-cell-liquid-policy")
))]
const VISIBLE_SURFACE_INDEX_MASK: u16 = 0x7fff;
#[cfg(feature = "renderer-cell-liquid-policy")]
const VISIBLE_SURFACE_INDEX_MASK: u16 = 0x3fff;
#[cfg(any(
    feature = "renderer-compact-cell-stream",
    feature = "renderer-cell-policy"
))]
const VISIBLE_INVARIANT_FRONT_BIT: u16 = 0x8000;
#[cfg(feature = "renderer-cell-liquid-policy")]
const VISIBLE_LIQUID_BIT: u16 = 0x4000;
#[cfg(any(
    feature = "renderer-compact-cell-stream",
    feature = "renderer-cell-policy"
))]
const _: () = assert!(MAX_FACE_COUNT <= VISIBLE_SURFACE_INDEX_MASK as usize + 1);
#[cfg(all(
    feature = "renderer-block-frustum",
    not(feature = "renderer-block-frustum-32")
))]
const VISIBLE_FACE_BLOCK_SIZE: usize = 16;
#[cfg(feature = "renderer-block-frustum-32")]
const VISIBLE_FACE_BLOCK_SIZE: usize = 32;
#[cfg(feature = "renderer-block-frustum")]
const MAX_VISIBLE_FACE_BLOCKS: usize = MAX_VISIBLE_FACE_COUNT.div_ceil(VISIBLE_FACE_BLOCK_SIZE);
#[cfg(feature = "renderer-hierarchical-block-frustum")]
const VISIBLE_FACE_SUPER_BLOCK_SIZE: usize = 4;
#[cfg(feature = "renderer-hierarchical-block-frustum")]
const MAX_VISIBLE_FACE_SUPER_BLOCKS: usize =
    MAX_VISIBLE_FACE_BLOCKS.div_ceil(VISIBLE_FACE_SUPER_BLOCK_SIZE);
/// Frame face indices carry this bit when the face's bounds reach behind the
/// near plane, so the world pass clips that face before submitting it.
const NEAR_FACE_BIT: u16 = 0x8000;
/// The selected liquid portal uses the PS1's AddQuarter blend and translucent
/// palette while retaining the same geometry and texture-window packets.
const WATER_BLEND_FACE_BIT: u16 = 0x4000;
const FRAME_FACE_INDEX_MASK: u16 = !(NEAR_FACE_BIT | WATER_BLEND_FACE_BIT);
/// Near plane distance in world units. Everything the player hull lets the
/// eye approach (walls at 16 units, ceilings at 10 above the eye) lies outside
/// the view cone at this depth, so clipping here removes no visible pixel;
/// vertices behind it would project through the GTE's zero-depth path and
/// draw as the warped and missing triangles seen when hugging a wall.
const NEAR_PLANE_UNITS: i32 = 8;
/// The same depth in view units: the loaded camera keeps the retained 3x
/// world scale.
const NEAR_PLANE_VIEW: i32 = NEAR_PLANE_UNITS * 3;
/// Largest face a near clip can grow by one vertex inside the batch scratch.
const NEAR_CLIP_MAX_VERTICES: usize = BATCH_MAX_VERTICES;
const BATCH_MAX_VERTICES: usize = 39;
const BATCH_MAX_SURFACES: usize = 13;
const SUBDIVISION_SCRATCH_VERTICES: usize = 12;
type BatchVertexStorage =
    [MaybeUninit<ClassicAffineVertex>; BATCH_MAX_VERTICES + SUBDIVISION_SCRATCH_VERTICES];
type BatchSurfaceStorage = [MaybeUninit<ClassicAffineBatchSurface>; BATCH_MAX_SURFACES];
#[cfg(any(feature = "renderer-census", feature = "renderer-subdivision-cache"))]
type BatchSourceSurfaceStorage = [MaybeUninit<u16>; BATCH_MAX_SURFACES];
#[cfg(feature = "renderer-fused-materialize-project")]
type BatchIndexedSourceStorage = [MaybeUninit<ClassicAffineIndexedBatchSource>; BATCH_MAX_SURFACES];
#[cfg(feature = "renderer-fused-materialize-project")]
type BatchVisibleIndexStorage = [MaybeUninit<u16>; BATCH_MAX_SURFACES];
#[cfg(feature = "renderer-static-world-reuse")]
const MAX_STATIC_WORLD_PACKET_SLOTS: usize = GPU_ARENA_WORDS / 10;
#[cfg(feature = "renderer-topology-cache")]
type ResidentBatchSurfaceStorage =
    [MaybeUninit<ClassicAffineResidentBatchSurface>; BATCH_MAX_SURFACES];

const MAX_ALIAS_VERTICES: usize = 512;
// Mirrored from `entity`: the verified Episode 1 high-water is 373.
const MAX_RENDER_ENTITIES: usize = 384;
// Episode 1 peaks at 1,230 visible leaves, or 154 bytes per row. The water
// portal is merged directly into this closed-corpus row, which stays far
// smaller than the previous generic 1,024-byte scratch allocation.
const MAX_VISIBILITY_BYTES: usize = 160;
#[cfg(feature = "renderer-topology-cache")]
const MAX_TOPOLOGY_CACHE_BATCHES: usize = 64;
const TEXTURE_CLUT_BASE_ROW: u16 = 240;
const LIQUID_CLUT_BASE_ROW: u16 = 246;
// Tuned brighter default: the cooker power is 0.8. This preserves the
// project's intentional lift without the row-three scene's green overcast.
const TEXTURE_GAMMA_LEVEL: u16 = 2;
/// The cooker lays one palette row per gamma power (1.0, 0.9, 0.8, 0.7, 0.6,
/// 0.5) under `TEXTURE_CLUT_BASE_ROW`; the Options menu's BRIGHTNESS picks
/// the row every textured packet in the frame references.
pub const BRIGHTNESS_LEVELS: u8 = 6;
static mut TEXTURE_GAMMA: u16 = TEXTURE_GAMMA_LEVEL;

/// The palette CLUT word for the current brightness level.
#[inline(always)]
fn clut_texture() -> u16 {
    (TEXTURE_CLUT_BASE_ROW + unsafe { TEXTURE_GAMMA }) << 6
}

#[inline(always)]
fn clut_liquid() -> u16 {
    (LIQUID_CLUT_BASE_ROW + unsafe { TEXTURE_GAMMA }) << 6
}

/// Select the palette gamma row: 0 is the untouched Quake palette, each step
/// lifts the mid-tones further (the original's brightness slider).
#[optimize(size)]
pub fn set_brightness_level(level: u8) {
    unsafe { TEXTURE_GAMMA = u16::from(level.min(BRIGHTNESS_LEVELS - 1)) };
}

/// `crosshair`. Held beside the gamma row for the same reason: the HUD pass
/// reads it every frame and nothing else does.
static mut CROSSHAIR: bool = true;

#[optimize(size)]
pub fn set_crosshair(on: bool) {
    unsafe { CROSSHAIR = on };
}

/// Fixed-resolution equivalent of the original `sb_lines` HUD depth.
static mut HUD_MODE: HudMode = DEFAULT_HUD_MODE;

#[optimize(size)]
pub fn set_hud_mode(mode: HudMode) {
    unsafe { HUD_MODE = mode };
}
const DUMMY_LIGHT_STYLE: usize = quake_core::lightstyle::DUMMY_STYLE;
// Two-level subdivision emits at most 19 packets for one source triangle;
// 13 words covers the larger textured-Gouraud quad packet.
const WORST_PACKET_WORDS_PER_TRIANGLE: usize = 19 * 13;
// A scoped windowed polygon adds its GP0(E2) selector and full-window reset.
const WORST_WINDOWED_PACKET_WORDS_PER_TRIANGLE: usize = 19 * 15;
const ALIAS_PACKET_WORDS: usize =
    core::mem::size_of::<ClassicTriTextured>() / core::mem::size_of::<u32>();
const ANIMATION_FRAMES_PER_SECOND: u32 = 30;
const LEGACY_SKY_SCROLL_TEXELS_PER_SECOND: u32 = 4;
const SKY_BACKGROUND_CYCLE_SECONDS: u32 = 16;
const SKY_FOREGROUND_CYCLE_SECONDS: u32 = 8;
// The masked cloud layer carries the sharp silhouette, so match the original
// renderer's 32-pixel horizontal span as closely as the PS1 budget permits.
// The sharp layer uses 32x20-pixel cells; the solid, slower background is
// deliberately half-resolution in each direction. This removes the visible
// dome folds without doubling the cost of both layers.
const SKY_FOREGROUND_COLUMNS: usize = 10;
const SKY_FOREGROUND_ROWS: usize = 12;
const SKY_FOREGROUND_CELLS: usize = SKY_FOREGROUND_COLUMNS * SKY_FOREGROUND_ROWS;
// The opaque layer must use the same direction sampling density as the
// transparent cloud layer.  A coarser background mesh is visible through the
// foreground's transparent texels and turns the dome's non-linear projection
// into large affine fans (most noticeably as a streak below screen centre).
const SKY_BACKGROUND_COLUMNS: usize = SKY_FOREGROUND_COLUMNS;
const SKY_BACKGROUND_ROWS: usize = SKY_FOREGROUND_ROWS;
const SKY_BACKGROUND_CELLS: usize = SKY_BACKGROUND_COLUMNS * SKY_BACKGROUND_ROWS;
const SKY_OT_SLOT: u32 = 2047;
const SKY_QUAD_WORDS: usize = 10;
const SKY_WINDOW_PACKET_WORDS: usize = 2;
const SKY_WINDOW_PACKET_COUNT: usize = 3;
const SKY_BACKGROUND_WORDS: usize = (SKY_FOREGROUND_CELLS + SKY_BACKGROUND_CELLS) * SKY_QUAD_WORDS
    + SKY_WINDOW_PACKET_COUNT * SKY_WINDOW_PACKET_WORDS;
// Every page that draws into this arena is a fixed string set with a bounded
// digit count. The peak is the Levels page plus the longest now-playing banner
// at 228 quads; 232 leaves four packets of measured margin. Each arena entry is
// 44 bytes and there are two frame arenas, so retaining the old 256 bound would
// reserve 2,112 bytes of shipping heap for unreachable packets. `push_text`
// drops glyphs rather than growing the vector, so a future miscount truncates a
// line rather than dangling a registered packet pointer.
const MENU_PACKET_CAPACITY: usize = 232;
// Full Classic worst case is 73 quads: both stone tiers, seven weapons, four
// ammo totals, keys, artifacts, sigils and three-digit lower fields. Ninety-six
// leaves 23 packets of margin, covers the denser text fallback, and avoids
// reserving several kilobytes of scarce shipping heap for unreachable packets.
const HUD_PACKET_CAPACITY: usize = 96;
// The closed shareware corpus peaks at the 64-glyph registration warning
// pinned in quake-core's text tests. Seventy-two leaves eight glyphs of margin
// and releases another 2,112 bytes across the two 44-byte frame arenas.
const CENTERPRINT_PACKET_CAPACITY: usize = 72;
/// Matches `survival::POWERUP_WARNING_TICKS` in whole seconds.
const POWERUP_WARNING_SECONDS: u8 = 3;
const VIEW_MODEL_PACKET_CAPACITY: usize = 384;
const FONT_TPAGE: u16 = 0x008f;
const MENU_TPAGE: u16 = 0x009f;
const QPLAQUE_SIZE: (u8, u8) = (32, 144);
const MAX_LIQUID_TEXTURES: usize = 4;
const MAX_RENDER_TEXTURES: usize = 128;
const LIQUID_WARP_BYTES: usize = MAX_LIQUID_TEXTURES * quake_core::liquid::LIQUID_TILE_BYTES;

/// The full-screen blends `quake_core::screenblend` can ask for: the sustained
/// contents murk, the transient flash, and the port's own level-transition
/// fade, which takes the last slot because it is registered separately and
/// after everything else on screen.
const SCREEN_TINT_CAPACITY: usize = 3;
/// Slot the transition fade owns, so it never collides with the two palette
/// blends `draw_screen_tints` fills from the front.
const SCREEN_FADE_SLOT: usize = SCREEN_TINT_CAPACITY - 1;

/// One full-screen semi-transparent quad, with its own blend mode.
///
/// PSoXide's screen-command list streams a packet's words straight to GP0, so
/// this carries its own GP0(E1) prefix and restores the draw mode after
/// itself, exactly the way `QuadTexturedMaterial` carries its own GP0(E2).
/// Without the restore the next untextured primitive would inherit this
/// packet's blend mode; every textured primitive in this renderer already
/// carries its own tpage word and is immune either way.
#[repr(C, align(4))]
#[derive(Copy, Clone)]
struct ScreenTintQuad {
    tag: u32,
    draw_mode: u32,
    color_cmd: u32,
    v0: u32,
    v1: u32,
    v2: u32,
    v3: u32,
    restore_draw_mode: u32,
}

impl ScreenTintQuad {
    const WORDS: u8 = 7;

    fn new(color: (u8, u8, u8), blend: BlendMode) -> Self {
        let material = TextureMaterial::blended(clut_texture(), FONT_TPAGE, color, blend);
        let restore = TextureMaterial::opaque(clut_texture(), FONT_TPAGE, (0x80, 0x80, 0x80));
        let vertex = |x: i16, y: i16| ((x as u16) as u32) | (((y as u16) as u32) << 16);
        let width = SCREEN_WIDTH;
        let height = SCREEN_HEIGHT;
        Self {
            tag: 0,
            draw_mode: material.draw_mode_word(),
            // Untextured semi-transparent flat quad: the blend equation comes
            // from the draw mode above and applies to the whole polygon.
            color_cmd: 0x2a00_0000
                | (color.0 as u32)
                | ((color.1 as u32) << 8)
                | ((color.2 as u32) << 16),
            v0: vertex(0, 0),
            v1: vertex(width, 0),
            v2: vertex(0, height),
            v3: vertex(width, height),
            restore_draw_mode: restore.draw_mode_word(),
        }
    }
}

const SCREEN_WIDTH: i16 = 320;
const SCREEN_HEIGHT: i16 = 240;

static mut GPU_ARENAS: [[u32; GPU_ARENA_WORDS]; 2] = [[0; GPU_ARENA_WORDS]; 2];
// A fixed `.bss` destination avoids constructing or growing a 16 KiB Vec on
// the PS1 bump heap. The immutable originals live with the resident map.
static mut LIQUID_WARP: [u8; LIQUID_WARP_BYTES] = [0; LIQUID_WARP_BYTES];

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Camera {
    pub origin: Vec3I32,
    pub angles: [i16; 3],
}

/// The near plane in world space: `forward . p >= threshold` keeps `p`.
#[derive(Copy, Clone)]
struct NearPlane {
    /// Q12 view direction (Quake's `AngleVectors` forward).
    forward: [i32; 3],
    /// `forward . eye + NEAR_PLANE_UNITS`, in Q12 world units.
    threshold: i32,
}

impl NearPlane {
    fn new(camera: Camera) -> Self {
        let pitch = camera.angles[0] as u16 & 0x0fff;
        let yaw = camera.angles[1] as u16 & 0x0fff;
        let (sin_pitch, cos_pitch) = (sin_q12(pitch), cos_q12(pitch));
        let (sin_yaw, cos_yaw) = (sin_q12(yaw), cos_q12(yaw));
        let forward = [
            mul_q12_i32(cos_pitch, cos_yaw),
            mul_q12_i32(cos_pitch, sin_yaw),
            -sin_pitch,
        ];
        // The eye rounds to whole units here; that shifts the plane by less
        // than a unit against an eight-unit margin.
        let eye = [
            camera.origin.x >> 12,
            camera.origin.y >> 12,
            camera.origin.z >> 12,
        ];
        let threshold = forward[0]
            .wrapping_mul(eye[0])
            .wrapping_add(forward[1].wrapping_mul(eye[1]))
            .wrapping_add(forward[2].wrapping_mul(eye[2]))
            .wrapping_add(NEAR_PLANE_UNITS << 12);
        Self { forward, threshold }
    }

    /// True when some corner of the box lies behind the plane, i.e. the box
    /// is not entirely in front of it.
    #[inline(always)]
    fn reaches_behind(&self, mins: [i16; 3], maxs: [i16; 3]) -> bool {
        let corner = |axis: usize| {
            i32::from(if self.forward[axis] > 0 {
                mins[axis]
            } else {
                maxs[axis]
            })
        };
        let dot = self.forward[0]
            .wrapping_mul(corner(0))
            .wrapping_add(self.forward[1].wrapping_mul(corner(1)))
            .wrapping_add(self.forward[2].wrapping_mul(corner(2)));
        dot < self.threshold
    }

    #[cfg(feature = "renderer-gte-near-classification")]
    fn as_aabb_clip_plane(self) -> AabbClipPlane {
        let normal = [
            self.forward[0] as i16,
            self.forward[1] as i16,
            self.forward[2] as i16,
        ];
        let signbits = u8::from(normal[0] < 0)
            | (u8::from(normal[1] < 0) << 1)
            | (u8::from(normal[2] < 0) << 2);
        AabbClipPlane {
            normal,
            kind: 3,
            signbits,
            distance: self.threshold,
        }
    }
}

/// One cooked Quake sprite frame. The cooker retains the original pixel size,
/// placement around the entity origin and texture coordinates in twelve bytes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct SpriteFrame {
    u: u8,
    v: u8,
    width: u8,
    height: u8,
    left: i16,
    up: i16,
    beam_length: i16,
    kind: u8,
}

impl SpriteFrame {
    #[inline(always)]
    fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; 12] = bytes.try_into().ok()?;
        let frame = Self {
            u: bytes[0],
            v: bytes[1],
            width: bytes[2],
            height: bytes[3],
            left: i16::from_le_bytes([bytes[4], bytes[5]]),
            up: i16::from_le_bytes([bytes[6], bytes[7]]),
            beam_length: i16::from_le_bytes([bytes[8], bytes[9]]),
            kind: bytes[10],
        };
        (frame.width != 0 && frame.height != 0 && frame.kind <= 4).then_some(frame)
    }
}

#[derive(Copy, Clone)]
struct SpriteSubmit {
    next: *mut u32,
    drawn: bool,
    overflow: bool,
}

/// Recreate `R_DrawSprite`'s five orientation modes in fixed point. Axes use
/// Quake's Q12 coordinates and signs, including the original world-upright
/// degeneracy when the view is vertical or directly above the entity.
#[inline(never)]
fn sprite_axes(
    kind: u8,
    origin: Vec3I32,
    angles: [i16; 3],
    camera: Camera,
) -> Option<(Vec3I32, Vec3I32, Vec3I32)> {
    const WORLD_UP: Vec3I32 = Vec3I32 {
        x: 0,
        y: 0,
        z: 4096,
    };
    let (camera_forward, camera_right, camera_up) = view_basis(camera.angles);
    let horizontal_unit = |x: i32, y: i32| {
        let length = isqrt_i32(square_i32_saturating(x).saturating_add(square_i32_saturating(y)));
        (length != 0).then(|| Vec3I32 {
            x: x.saturating_mul(4096) / length,
            y: y.saturating_mul(4096) / length,
            z: 0,
        })
    };
    match kind {
        // SPR_VP_PARALLEL_UPRIGHT
        0 => {
            let right = horizontal_unit(camera_forward.y, -camera_forward.x)?;
            let forward = Vec3I32 {
                x: -right.y,
                y: right.x,
                z: 0,
            };
            Some((forward, right, WORLD_UP))
        }
        // SPR_FACING_UPRIGHT
        1 => {
            let toward_sprite_x = origin.x.saturating_sub(camera.origin.x) >> 12;
            let toward_sprite_y = origin.y.saturating_sub(camera.origin.y) >> 12;
            let right = horizontal_unit(toward_sprite_y, -toward_sprite_x)?;
            let forward = Vec3I32 {
                x: -right.y,
                y: right.x,
                z: 0,
            };
            Some((forward, right, WORLD_UP))
        }
        // SPR_VP_PARALLEL
        2 => Some((camera_forward, camera_right, camera_up)),
        // SPR_ORIENTED
        3 => Some(view_basis(angles)),
        // SPR_VP_PARALLEL_ORIENTED
        4 => {
            let roll = angles[2] as u16 & 0x0fff;
            let (sr, cr) = (sin_q12(roll), cos_q12(roll));
            let combine =
                |first: Vec3I32, first_scale: i32, second: Vec3I32, second_scale: i32| Vec3I32 {
                    x: mul_q12_i32(first.x, first_scale)
                        .saturating_add(mul_q12_i32(second.x, second_scale)),
                    y: mul_q12_i32(first.y, first_scale)
                        .saturating_add(mul_q12_i32(second.y, second_scale)),
                    z: mul_q12_i32(first.z, first_scale)
                        .saturating_add(mul_q12_i32(second.z, second_scale)),
                };
            Some((
                camera_forward,
                combine(camera_right, cr, camera_up, sr),
                combine(camera_right, -sr, camera_up, cr),
            ))
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn draw_sprite_model(
    model: AliasModelView<'_>,
    frame_index: usize,
    origin: Vec3I32,
    angles: [i16; 3],
    camera: Camera,
    view: QuakeViewTransform,
    output: *mut u32,
    end: *mut u32,
) -> SpriteSubmit {
    const PACKET_WORDS: usize = QuadTextured::WORDS as usize + 1;
    let header = model.header();
    let frame_index = frame_index.min(header.frame_count as usize - 1);
    let Some(frame) = model.frame_bytes(frame_index).and_then(SpriteFrame::decode) else {
        return SpriteSubmit {
            next: output,
            drawn: false,
            overflow: false,
        };
    };
    let Some((forward, right, up)) = sprite_axes(frame.kind, origin, angles, camera) else {
        return SpriteSubmit {
            next: output,
            drawn: false,
            overflow: false,
        };
    };

    // Quake culls the poster's back before shifting a nonzero beam length.
    let modelorg = Vec3I32 {
        x: camera.origin.x.saturating_sub(origin.x) >> 12,
        y: camera.origin.y.saturating_sub(origin.y) >> 12,
        z: camera.origin.z.saturating_sub(origin.z) >> 12,
    };
    let facing = forward
        .x
        .saturating_mul(modelorg.x)
        .saturating_add(forward.y.saturating_mul(modelorg.y))
        .saturating_add(forward.z.saturating_mul(modelorg.z));
    if facing >= 0 {
        return SpriteSubmit {
            next: output,
            drawn: false,
            overflow: false,
        };
    }
    if !packet_capacity(output, end, PACKET_WORDS) {
        return SpriteSubmit {
            next: output,
            drawn: false,
            overflow: true,
        };
    }

    let center = Vec3I32 {
        x: origin
            .x
            .saturating_sub(forward.x.saturating_mul(i32::from(frame.beam_length))),
        y: origin
            .y
            .saturating_sub(forward.y.saturating_mul(i32::from(frame.beam_length))),
        z: origin
            .z
            .saturating_sub(forward.z.saturating_mul(i32::from(frame.beam_length))),
    };
    let right_edge = i32::from(frame.left).saturating_add(i32::from(frame.width));
    let down = i32::from(frame.up).saturating_sub(i32::from(frame.height));
    let corner = |horizontal: i32, vertical: i32| Vec3I32 {
        x: center
            .x
            .saturating_add(right.x.saturating_mul(horizontal))
            .saturating_add(up.x.saturating_mul(vertical)),
        y: center
            .y
            .saturating_add(right.y.saturating_mul(horizontal))
            .saturating_add(up.y.saturating_mul(vertical)),
        z: center
            .z
            .saturating_add(right.z.saturating_mul(horizontal))
            .saturating_add(up.z.saturating_mul(vertical)),
    };
    let vertex = |point: Vec3I32| {
        GteVec3I16::new(
            (point.x >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            (point.y >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            (point.z >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        )
    };
    scene::load_rotation(&view.rotation);
    scene::load_translation(view.translation);
    let projected = [
        scene::project_vertex(vertex(corner(i32::from(frame.left), i32::from(frame.up)))),
        scene::project_vertex(vertex(corner(right_edge, i32::from(frame.up)))),
        scene::project_vertex(vertex(corner(i32::from(frame.left), down))),
        scene::project_vertex(vertex(corner(right_edge, down))),
    ];
    if projected.iter().any(|vertex| vertex.sz == 0) {
        return SpriteSubmit {
            next: output,
            drawn: false,
            overflow: false,
        };
    }
    let u_right = frame.u.wrapping_add(frame.width - 1);
    let v_bottom = frame.v.wrapping_add(frame.height - 1);
    let mut packet = QuadTextured::new(
        projected.map(|vertex| (vertex.sx, vertex.sy)),
        [
            (frame.u, frame.v),
            (u_right, frame.v),
            (frame.u, v_bottom),
            (u_right, v_bottom),
        ],
        clut_texture(),
        header.skins[0].texture_page,
        (0x80, 0x80, 0x80),
    );
    packet.tag = u32::from(QuadTextured::WORDS) << 24;
    unsafe { output.cast::<QuadTextured>().write(packet) };
    SpriteSubmit {
        next: unsafe { output.add(PACKET_WORDS) },
        drawn: true,
        overflow: false,
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ViewModelInput {
    pub weapon: WeaponView,
    pub velocity: Vec3I32,
    pub elapsed_ticks: u16,
    pub muzzle_flash: bool,
    /// `V_CalcBob` for this frame, Q12 Quake units; the camera already
    /// carries the same value in its eye height.
    pub bob_q12: i32,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderStats {
    pub visible_faces: u16,
    pub visible_entities: u16,
    pub alias_packets: u32,
    pub sprite_packets: u32,
    pub projectile_entities: u16,
    pub pvs_projectile_entities: u16,
    pub visible_projectile_entities: u16,
    pub projectile_packets: u32,
    pub nail_projectile_packets: u32,
    pub grenade_projectile_packets: u32,
    pub rocket_projectile_packets: u32,
    pub lightning_beam_packets: u32,
    pub explosion_effect_packets: u32,
    pub impact_particle_packets: u32,
    pub view_model_packets: u32,
    pub hud_packets: u32,
    #[cfg(feature = "visual-parity-regression")]
    pub view_model_registered_packets: u32,
    #[cfg(feature = "visual-parity-regression")]
    pub hud_registered_packets: u32,
    #[cfg(feature = "visual-parity-regression")]
    pub crosshair_registered_packets: u32,
    #[cfg(feature = "visual-parity-regression")]
    pub screen_registered_packets: u32,
    pub packets: u32,
    pub hardware_triangles: u32,
    pub packet_overflow_avoided: bool,
    #[cfg(feature = "renderer-topology-cache")]
    pub topology_cache_hits: u32,
    #[cfg(feature = "renderer-topology-cache")]
    pub topology_cache_misses: u32,
    #[cfg(feature = "renderer-topology-cache")]
    pub topology_invariant_hit_slots: u32,
    #[cfg(feature = "renderer-topology-cache")]
    pub topology_invariant_miss_slots: u32,
    #[cfg(feature = "renderer-indexed-projection")]
    pub indexed_projection_corners: u32,
    #[cfg(feature = "renderer-indexed-projection")]
    pub indexed_projection_unique: u32,
    #[cfg(feature = "renderer-subdivision-cache")]
    pub subdivision_cache_hits: u32,
    #[cfg(feature = "renderer-subdivision-cache")]
    pub subdivision_cache_allocations: u32,
    #[cfg(feature = "renderer-subdivision-cache")]
    pub subdivision_cache_replacements: u32,
    #[cfg(feature = "renderer-subdivision-cache")]
    pub subdivision_cache_fallbacks: u32,
    #[cfg(feature = "renderer-subdivision-cache")]
    pub subdivision_cache_initializations: u32,
    #[cfg(feature = "renderer-subdivision-cache")]
    pub subdivision_cache_packets: u32,
    /// Exact high-water of this frame's double-buffered world/entity packet
    /// arena. The map-route probe retains the maximum across Episode 1.
    #[cfg(feature = "episode1-regression")]
    pub packet_arena_words: u32,
    #[cfg(feature = "visual-parity-regression")]
    pub scoped_window_packets: u32,
    #[cfg(feature = "visual-parity-regression")]
    pub scoped_window_resets: u32,
    #[cfg(feature = "visual-parity-regression")]
    pub scoped_window_reset_failures: u32,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct HudPacketStats {
    generated: u32,
    hud_registered: u32,
    crosshair_registered: u32,
}

/// Render-only face and plane fields decoded when the PVS changes.
///
/// PSB3 deliberately keeps compact, misaligned wire records to save disc and
/// resident RAM. Decoding those records for every visible face on every frame
/// is pure repetition. The shared compact plane preserves the authored
/// normal, distance, and axial class while dropping padding and an unused
/// wide class field. Bounds use the cross-engine retained-surface contract.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct VisibleFace {
    #[cfg(not(feature = "renderer-compact-cell-stream"))]
    plane: CompactPlane,
    bounds: RetainedSurfaceBounds,
    face: CookedDrawSurface,
}

#[cfg(not(feature = "renderer-compact-cell-stream"))]
const _: [(); 36] = [(); core::mem::size_of::<VisibleFace>()];
#[cfg(feature = "renderer-compact-cell-stream")]
const _: [(); 24] = [(); core::mem::size_of::<VisibleFace>()];

/// Conservative union bounds for one consecutive visible-face block.
#[cfg(feature = "renderer-block-frustum")]
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct VisibleFaceBlock {
    mins: [i16; 3],
    maxs: [i16; 3],
}

#[cfg(feature = "renderer-block-frustum")]
const _: [(); 12] = [(); core::mem::size_of::<VisibleFaceBlock>()];

/// Diagnostic-only counters for renderer structure experiments. The feature
/// that enables these records adds several full passes and a debug write per
/// frame, so none of this is present in a normal or timing build.
#[cfg(feature = "renderer-census")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct SelectionCensus {
    pvs_faces: u32,
    policy_rejects: u32,
    backface_rejects: u32,
    frustum_rejects: u32,
    selected_faces: u32,
    water_blend_faces: u32,
    plane_tests: u32,
    plane_run_tests: u32,
    plane_tests_saved: u32,
    max_plane_run: u32,
}

#[cfg(feature = "renderer-census")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct BlockCensus {
    groups: u32,
    rejected_groups: u32,
    rejected_faces: u32,
    aabb_tests_saved: u32,
}

#[cfg(feature = "renderer-census")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct ProjectionCensus {
    candidate_corners: u32,
    unique_positions: u32,
    batches: u32,
    previous_face_reuses: u32,
    previous_two_face_reuses: u32,
    near_corners: u32,
    special_corners: u32,
    layered_sky_corners: u32,
    oversized_corners: u32,
    ordinary_base_packet_bytes: u32,
    resident_template_faces: u32,
    resident_template_packet_bytes: u32,
    dynamic_light_template_reject_bytes: u32,
}

#[cfg(feature = "renderer-census")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct RendererCensus {
    selection: SelectionCensus,
    near_faces: u32,
    blocks: [BlockCensus; 3],
    projection: ProjectionCensus,
    visibility_rebuilt: u32,
    leaf: u32,
    portal_leaf: u32,
    selected_hash_a: u32,
    selected_hash_b: u32,
    ordinary_output_packet_bytes: u32,
    ordinary_output_packets: u32,
    ordinary_output_hardware_triangles: u32,
    topology: ClassicAffineTopologyCensus,
    subdivision_slab_caches: [SubdivisionCacheFrame; SUBDIVISION_CACHE_BUDGETS_KIB.len()],
    packet_arena_words: u32,
    emitted_packets: u32,
    hardware_triangles: u32,
    packet_overflow_avoided: u32,
}

#[cfg(feature = "renderer-census")]
const SUBDIVISION_LEVEL1_SLOT_BYTES: usize = 252;
#[cfg(feature = "renderer-census")]
const SUBDIVISION_LEVEL2_SLOT_BYTES: usize = 748;
#[cfg(feature = "renderer-census")]
const SUBDIVISION_CACHE_BUDGETS_KIB: [usize; 4] = [16, 32, 48, 64];
#[cfg(feature = "renderer-census")]
const SUBDIVISION_SLAB_LEVEL1_MAX_SLOTS: usize =
    SUBDIVISION_CACHE_BUDGETS_KIB[SUBDIVISION_CACHE_BUDGETS_KIB.len() - 1] * 1024 * 3
        / 5
        / SUBDIVISION_LEVEL1_SLOT_BYTES;
#[cfg(feature = "renderer-census")]
const SUBDIVISION_SLAB_LEVEL2_MAX_SLOTS: usize =
    (SUBDIVISION_CACHE_BUDGETS_KIB[SUBDIVISION_CACHE_BUDGETS_KIB.len() - 1] * 1024
        - SUBDIVISION_SLAB_LEVEL1_MAX_SLOTS * SUBDIVISION_LEVEL1_SLOT_BYTES)
        / SUBDIVISION_LEVEL2_SLOT_BYTES;
#[cfg(feature = "renderer-census")]
const SUBDIVISION_REQUEST_CAPACITY: usize = BATCH_MAX_VERTICES;

#[cfg(feature = "renderer-census")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct SubdivisionCacheFrame {
    requests: u32,
    hits: u32,
    allocations: u32,
    replacements: u32,
    fallbacks: u32,
    resident: u32,
    requested_packet_bytes: u32,
    hit_packet_bytes: u32,
    hit_invariant_bytes: u32,
}

#[cfg(feature = "renderer-census")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct SubdivisionCacheKey {
    map_generation: u32,
    material: u32,
    source_face: u16,
    root: u8,
    level: u8,
    underdraw: u8,
}

#[cfg(feature = "renderer-census")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct SubdivisionCacheSlot {
    key: SubdivisionCacheKey,
    last_used_frame: u32,
    screen_z: u16,
    valid: bool,
}

#[cfg(feature = "renderer-census")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct SubdivisionSlabCacheModel {
    level1_slots: [SubdivisionCacheSlot; SUBDIVISION_SLAB_LEVEL1_MAX_SLOTS],
    level2_slots: [SubdivisionCacheSlot; SUBDIVISION_SLAB_LEVEL2_MAX_SLOTS],
    level1_capacity: usize,
    level2_capacity: usize,
    map_generation: u32,
}

#[cfg(feature = "renderer-census")]
impl SubdivisionSlabCacheModel {
    const fn new(budget_bytes: usize) -> Self {
        let level1_capacity = budget_bytes * 3 / 5 / SUBDIVISION_LEVEL1_SLOT_BYTES;
        let level2_capacity = (budget_bytes - level1_capacity * SUBDIVISION_LEVEL1_SLOT_BYTES)
            / SUBDIVISION_LEVEL2_SLOT_BYTES;
        Self {
            level1_slots: [SubdivisionCacheSlot {
                key: SubdivisionCacheKey {
                    map_generation: 0,
                    material: 0,
                    source_face: 0,
                    root: 0,
                    level: 0,
                    underdraw: 0,
                },
                last_used_frame: 0,
                screen_z: 0,
                valid: false,
            }; SUBDIVISION_SLAB_LEVEL1_MAX_SLOTS],
            level2_slots: [SubdivisionCacheSlot {
                key: SubdivisionCacheKey {
                    map_generation: 0,
                    material: 0,
                    source_face: 0,
                    root: 0,
                    level: 0,
                    underdraw: 0,
                },
                last_used_frame: 0,
                screen_z: 0,
                valid: false,
            }; SUBDIVISION_SLAB_LEVEL2_MAX_SLOTS],
            level1_capacity,
            level2_capacity,
            map_generation: u32::MAX,
        }
    }

    fn begin_frame(&mut self, map_generation: u32) {
        if self.map_generation != map_generation {
            for slot in &mut self.level1_slots[..self.level1_capacity] {
                slot.valid = false;
            }
            for slot in &mut self.level2_slots[..self.level2_capacity] {
                slot.valid = false;
            }
            self.map_generation = map_generation;
        }
    }

    fn request(
        &mut self,
        frame: u32,
        source_face: u16,
        request: ClassicAffineSubdivisionRequest,
        counters: &mut SubdivisionCacheFrame,
    ) {
        let key = SubdivisionCacheKey {
            map_generation: self.map_generation,
            material: request.material,
            source_face,
            root: request.root,
            level: request.level,
            underdraw: request.underdraw,
        };
        let slots = if request.level == 1 {
            &mut self.level1_slots[..self.level1_capacity]
        } else {
            &mut self.level2_slots[..self.level2_capacity]
        };
        request_subdivision_slot(slots, frame, key, request, counters);
        counters.resident = self.resident_count();
    }

    fn resident_count(&self) -> u32 {
        self.level1_slots[..self.level1_capacity]
            .iter()
            .chain(self.level2_slots[..self.level2_capacity].iter())
            .filter(|slot| slot.valid)
            .count() as u32
    }
}

#[cfg(feature = "renderer-census")]
fn request_subdivision_slot(
    slots: &mut [SubdivisionCacheSlot],
    frame: u32,
    key: SubdivisionCacheKey,
    request: ClassicAffineSubdivisionRequest,
    counters: &mut SubdivisionCacheFrame,
) {
    counters.requests = counters.requests.wrapping_add(1);
    counters.requested_packet_bytes = counters
        .requested_packet_bytes
        .wrapping_add(u32::from(request.packet_bytes));
    if let Some(slot) = slots.iter_mut().find(|slot| slot.valid && slot.key == key) {
        slot.last_used_frame = frame;
        slot.screen_z = request.otz;
        counters.hits = counters.hits.wrapping_add(1);
        counters.hit_packet_bytes = counters
            .hit_packet_bytes
            .wrapping_add(u32::from(request.packet_bytes));
        counters.hit_invariant_bytes = counters
            .hit_invariant_bytes
            .wrapping_add(u32::from(request.invariant_bytes));
        return;
    }
    let empty = slots.iter().position(|slot| !slot.valid);
    let mut victim = None;
    if empty.is_none() {
        for (index, slot) in slots.iter().enumerate() {
            let age = frame.wrapping_sub(slot.last_used_frame);
            if age < 2 {
                continue;
            }
            let replace = victim.is_none_or(|current: usize| {
                let current_slot = &slots[current];
                slot.screen_z > current_slot.screen_z
                    || (slot.screen_z == current_slot.screen_z
                        && age > frame.wrapping_sub(current_slot.last_used_frame))
            });
            if replace {
                victim = Some(index);
            }
        }
    }
    let Some(slot_index) = empty.or(victim) else {
        counters.fallbacks = counters.fallbacks.wrapping_add(1);
        return;
    };
    let replacing = slots[slot_index].valid;
    slots[slot_index] = SubdivisionCacheSlot {
        key,
        last_used_frame: frame,
        screen_z: request.otz,
        valid: true,
    };
    counters.allocations = counters.allocations.wrapping_add(1);
    counters.replacements = counters.replacements.wrapping_add(u32::from(replacing));
}

#[cfg(feature = "renderer-subdivision-cache")]
const RESIDENT_SUBDIVISION_BYTES_PER_POOL: usize = 48 * 1024;
#[cfg(feature = "renderer-subdivision-cache")]
const RESIDENT_SUBDIVISION_LEVEL1_BYTES: usize = 252;
#[cfg(feature = "renderer-subdivision-cache")]
const RESIDENT_SUBDIVISION_LEVEL2_BYTES: usize = 748;
#[cfg(all(
    feature = "renderer-subdivision-cache",
    not(feature = "renderer-subdivision-cache-level2")
))]
const RESIDENT_SUBDIVISION_LEVEL1_SLOTS: usize = 117;
#[cfg(feature = "renderer-subdivision-cache-level2")]
const RESIDENT_SUBDIVISION_LEVEL1_SLOTS: usize = 0;
#[cfg(all(
    feature = "renderer-subdivision-cache",
    not(feature = "renderer-subdivision-cache-level2")
))]
const RESIDENT_SUBDIVISION_LEVEL2_SLOTS: usize = 26;
#[cfg(all(
    feature = "renderer-subdivision-cache-level2",
    not(feature = "renderer-subdivision-cache-level2-small")
))]
const RESIDENT_SUBDIVISION_LEVEL2_SLOTS: usize =
    RESIDENT_SUBDIVISION_BYTES_PER_POOL / RESIDENT_SUBDIVISION_LEVEL2_BYTES;
#[cfg(feature = "renderer-subdivision-cache-level2-small")]
const RESIDENT_SUBDIVISION_LEVEL2_SLOTS: usize = 26;
#[cfg(all(
    feature = "renderer-subdivision-cache",
    not(feature = "renderer-subdivision-cache-level2")
))]
const RESIDENT_SUBDIVISION_LEVEL1_DIRECTORY_SLOTS: usize = 256;
#[cfg(feature = "renderer-subdivision-cache-level2")]
const RESIDENT_SUBDIVISION_LEVEL1_DIRECTORY_SLOTS: usize = 1;
#[cfg(all(
    feature = "renderer-subdivision-cache",
    not(feature = "renderer-subdivision-cache-level2")
))]
const RESIDENT_SUBDIVISION_LEVEL2_DIRECTORY_SLOTS: usize = 64;
#[cfg(all(
    feature = "renderer-subdivision-cache-level2",
    not(feature = "renderer-subdivision-cache-level2-small")
))]
const RESIDENT_SUBDIVISION_LEVEL2_DIRECTORY_SLOTS: usize = 128;
#[cfg(feature = "renderer-subdivision-cache-level2-small")]
const RESIDENT_SUBDIVISION_LEVEL2_DIRECTORY_SLOTS: usize = 64;
#[cfg(feature = "renderer-subdivision-cache")]
const RESIDENT_SUBDIVISION_DIRECTORY_EMPTY: u16 = u16::MAX;
#[cfg(feature = "renderer-subdivision-cache")]
const RESIDENT_SUBDIVISION_DIRECTORY_TOMBSTONE: u16 = u16::MAX - 1;
#[cfg(feature = "renderer-subdivision-cache")]
const RESIDENT_SUBDIVISION_LEVEL1_WORDS: usize = RESIDENT_SUBDIVISION_LEVEL1_BYTES / 4;
#[cfg(feature = "renderer-subdivision-cache")]
const RESIDENT_SUBDIVISION_LEVEL2_WORDS: usize = RESIDENT_SUBDIVISION_LEVEL2_BYTES / 4;
#[cfg(feature = "renderer-resident-base-cache")]
const RESIDENT_BASE_PACKET_SLOTS: usize = 512;
#[cfg(not(feature = "renderer-resident-base-cache"))]
const RESIDENT_BASE_PACKET_SLOTS: usize = 0;
#[cfg(feature = "renderer-subdivision-cache")]
const RESIDENT_BASE_PACKET_WORDS: usize = 52 / 4;
#[cfg(feature = "renderer-subdivision-cache")]
const RESIDENT_SUBDIVISION_USED_WORDS: usize = RESIDENT_SUBDIVISION_LEVEL1_SLOTS
    * RESIDENT_SUBDIVISION_LEVEL1_WORDS
    + RESIDENT_SUBDIVISION_LEVEL2_SLOTS * RESIDENT_SUBDIVISION_LEVEL2_WORDS
    + RESIDENT_BASE_PACKET_SLOTS * RESIDENT_BASE_PACKET_WORDS;
// Only the address-control build deliberately retains the original 48 KiB
// boundary. Other cache shapes must return unused slab capacity to the
// dynamic stream: reserving the entire modelling budget for a 26-slot L2
// cache leaves just 80 KiB for ordinary/L1/model/HUD packets and can drop the
// view model before the first cache hit.
#[cfg(all(
    feature = "renderer-subdivision-cache",
    feature = "renderer-subdivision-cache-level2-layout-control"
))]
const RESIDENT_SUBDIVISION_POOL_WORDS: usize = RESIDENT_SUBDIVISION_BYTES_PER_POOL / 4;
#[cfg(all(
    feature = "renderer-subdivision-cache",
    not(feature = "renderer-subdivision-cache-level2-layout-control")
))]
const RESIDENT_SUBDIVISION_POOL_WORDS: usize = RESIDENT_SUBDIVISION_USED_WORDS;
#[cfg(feature = "renderer-subdivision-cache")]
const DYNAMIC_GPU_ARENA_WORDS: usize = GPU_ARENA_WORDS - RESIDENT_SUBDIVISION_POOL_WORDS;
#[cfg(feature = "renderer-subdivision-cache")]
const _: () = assert!(RESIDENT_SUBDIVISION_USED_WORDS <= RESIDENT_SUBDIVISION_POOL_WORDS);
#[cfg(all(
    feature = "renderer-subdivision-cache-level2-small",
    not(feature = "renderer-resident-base-cache")
))]
const _: () = assert!(
    DYNAMIC_GPU_ARENA_WORDS == GPU_ARENA_WORDS - 26 * (RESIDENT_SUBDIVISION_LEVEL2_BYTES / 4)
);
#[cfg(feature = "renderer-resident-base-cache")]
const _: () = assert!(
    DYNAMIC_GPU_ARENA_WORDS
        == GPU_ARENA_WORDS
            - 26 * (RESIDENT_SUBDIVISION_LEVEL2_BYTES / 4)
            - RESIDENT_BASE_PACKET_SLOTS * RESIDENT_BASE_PACKET_WORDS
);

#[cfg(feature = "renderer-subdivision-cache")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct ResidentSubdivisionKey {
    material: u32,
    source_face: u16,
    root: u8,
    level: u8,
    underdraw: u8,
}

#[cfg(feature = "renderer-subdivision-cache")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct ResidentSubdivisionSlot {
    key: ResidentSubdivisionKey,
    last_used_frame: u32,
    screen_z: u16,
    initialized_pools: u8,
    valid: bool,
}

#[cfg(feature = "renderer-subdivision-cache")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ResidentSubdivisionCache {
    level1: [ResidentSubdivisionSlot; RESIDENT_SUBDIVISION_LEVEL1_SLOTS],
    level2: [ResidentSubdivisionSlot; RESIDENT_SUBDIVISION_LEVEL2_SLOTS],
    base_packets: [ResidentSubdivisionSlot; RESIDENT_BASE_PACKET_SLOTS],
    level1_directory: [u16; RESIDENT_SUBDIVISION_LEVEL1_DIRECTORY_SLOTS],
    level2_directory: [u16; RESIDENT_SUBDIVISION_LEVEL2_DIRECTORY_SLOTS],
    map_generation: u32,
}

#[cfg(feature = "renderer-subdivision-cache")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ResidentSubdivisionAcquire {
    class_offset_words: usize,
    resident: bool,
    logical_hit: bool,
    replaced: bool,
}

#[cfg(feature = "renderer-subdivision-cache")]
impl ResidentSubdivisionCache {
    const fn new() -> Self {
        const EMPTY: ResidentSubdivisionSlot = ResidentSubdivisionSlot {
            key: ResidentSubdivisionKey {
                material: 0,
                source_face: 0,
                root: 0,
                level: 0,
                underdraw: 0,
            },
            last_used_frame: 0,
            screen_z: 0,
            initialized_pools: 0,
            valid: false,
        };
        Self {
            level1: [EMPTY; RESIDENT_SUBDIVISION_LEVEL1_SLOTS],
            level2: [EMPTY; RESIDENT_SUBDIVISION_LEVEL2_SLOTS],
            base_packets: [EMPTY; RESIDENT_BASE_PACKET_SLOTS],
            level1_directory: [RESIDENT_SUBDIVISION_DIRECTORY_EMPTY;
                RESIDENT_SUBDIVISION_LEVEL1_DIRECTORY_SLOTS],
            level2_directory: [RESIDENT_SUBDIVISION_DIRECTORY_EMPTY;
                RESIDENT_SUBDIVISION_LEVEL2_DIRECTORY_SLOTS],
            map_generation: u32::MAX,
        }
    }

    fn begin_map(&mut self, map_generation: u32) {
        if self.map_generation != map_generation {
            for slot in &mut self.level1 {
                slot.valid = false;
                slot.initialized_pools = 0;
            }
            for slot in &mut self.level2 {
                slot.valid = false;
                slot.initialized_pools = 0;
            }
            for slot in &mut self.base_packets {
                slot.valid = false;
                slot.initialized_pools = 0;
            }
            self.level1_directory
                .fill(RESIDENT_SUBDIVISION_DIRECTORY_EMPTY);
            self.level2_directory
                .fill(RESIDENT_SUBDIVISION_DIRECTORY_EMPTY);
            self.map_generation = map_generation;
        }
    }

    #[cfg(feature = "renderer-resident-base-cache")]
    #[inline(always)]
    fn acquire_base_packet(
        &mut self,
        active_pool: usize,
        frame: u32,
        key: ResidentSubdivisionKey,
        screen_z: u16,
    ) -> Option<ResidentSubdivisionAcquire> {
        const WAYS: usize = 4;
        const SETS: usize = RESIDENT_BASE_PACKET_SLOTS / WAYS;
        debug_assert!(SETS.is_power_of_two());
        let first = (resident_subdivision_key_hash(key) & (SETS - 1)) * WAYS;
        let mut hit = None;
        let mut way = 0usize;
        while way < WAYS {
            let index = first + way;
            if self.base_packets[index].valid && self.base_packets[index].key == key {
                hit = Some(index);
                break;
            }
            way += 1;
        }
        let pool_bit = 1u8 << active_pool;
        if let Some(index) = hit {
            let slot = &mut self.base_packets[index];
            let resident = slot.initialized_pools & pool_bit != 0;
            slot.initialized_pools |= pool_bit;
            slot.last_used_frame = frame;
            slot.screen_z = screen_z;
            return Some(ResidentSubdivisionAcquire {
                class_offset_words: RESIDENT_SUBDIVISION_LEVEL1_SLOTS
                    * RESIDENT_SUBDIVISION_LEVEL1_WORDS
                    + RESIDENT_SUBDIVISION_LEVEL2_SLOTS * RESIDENT_SUBDIVISION_LEVEL2_WORDS
                    + index * RESIDENT_BASE_PACKET_WORDS,
                resident,
                logical_hit: true,
                replaced: false,
            });
        }

        let mut victim = None;
        way = 0;
        while way < WAYS {
            let index = first + way;
            let slot = &self.base_packets[index];
            if !slot.valid {
                victim = Some(index);
                break;
            }
            if slot.last_used_frame != frame {
                let replace = victim.is_none_or(|current: usize| {
                    frame.wrapping_sub(slot.last_used_frame)
                        > frame.wrapping_sub(self.base_packets[current].last_used_frame)
                });
                if replace {
                    victim = Some(index);
                }
            }
            way += 1;
        }
        // Every way in this set may already be linked in the active build
        // buffer. In that case fallback is mandatory: replacement would
        // mutate a packet reachable by the live ordering table.
        let index = victim?;
        let slot = &mut self.base_packets[index];
        let replaced = slot.valid;
        *slot = ResidentSubdivisionSlot {
            key,
            last_used_frame: frame,
            screen_z,
            initialized_pools: pool_bit,
            valid: true,
        };
        Some(ResidentSubdivisionAcquire {
            class_offset_words: RESIDENT_SUBDIVISION_LEVEL1_SLOTS
                * RESIDENT_SUBDIVISION_LEVEL1_WORDS
                + RESIDENT_SUBDIVISION_LEVEL2_SLOTS * RESIDENT_SUBDIVISION_LEVEL2_WORDS
                + index * RESIDENT_BASE_PACKET_WORDS,
            resident: false,
            logical_hit: false,
            replaced,
        })
    }

    fn acquire(
        &mut self,
        active_pool: usize,
        frame: u32,
        key: ResidentSubdivisionKey,
        screen_z: u16,
    ) -> Option<ResidentSubdivisionAcquire> {
        let (slots, directory, base_words, slot_words) = if key.level == 1 {
            (
                &mut self.level1[..],
                &mut self.level1_directory[..],
                0,
                RESIDENT_SUBDIVISION_LEVEL1_WORDS,
            )
        } else {
            (
                &mut self.level2[..],
                &mut self.level2_directory[..],
                RESIDENT_SUBDIVISION_LEVEL1_SLOTS * RESIDENT_SUBDIVISION_LEVEL1_WORDS,
                RESIDENT_SUBDIVISION_LEVEL2_WORDS,
            )
        };
        let pool_bit = 1u8 << active_pool;
        if let Some(index) = resident_subdivision_directory_find(directory, slots, key) {
            let slot = &mut slots[index];
            let resident = slot.initialized_pools & pool_bit != 0;
            slot.initialized_pools |= pool_bit;
            slot.last_used_frame = frame;
            slot.screen_z = screen_z;
            return Some(ResidentSubdivisionAcquire {
                class_offset_words: base_words + index * slot_words,
                resident,
                logical_hit: true,
                replaced: false,
            });
        }

        let empty = slots.iter().position(|slot| !slot.valid);
        let mut victim = None;
        if empty.is_none() {
            for (index, slot) in slots.iter().enumerate() {
                let age = frame.wrapping_sub(slot.last_used_frame);
                if age < 2 {
                    continue;
                }
                let replace = victim.is_none_or(|current: usize| {
                    let current_slot = &slots[current];
                    slot.screen_z > current_slot.screen_z
                        || (slot.screen_z == current_slot.screen_z
                            && age > frame.wrapping_sub(current_slot.last_used_frame))
                });
                if replace {
                    victim = Some(index);
                }
            }
        }
        let index = empty.or(victim)?;
        let replaced = slots[index].valid;
        if replaced {
            resident_subdivision_directory_remove(directory, slots, slots[index].key, index);
        }
        slots[index] = ResidentSubdivisionSlot {
            key,
            last_used_frame: frame,
            screen_z,
            initialized_pools: pool_bit,
            valid: true,
        };
        resident_subdivision_directory_insert(directory, key, index);
        Some(ResidentSubdivisionAcquire {
            class_offset_words: base_words + index * slot_words,
            resident: false,
            logical_hit: false,
            replaced,
        })
    }
}

#[cfg(feature = "renderer-subdivision-cache")]
#[inline(always)]
fn resident_subdivision_key_hash(key: ResidentSubdivisionKey) -> usize {
    let mut hash = key.material
        ^ (u32::from(key.source_face) << 8)
        ^ (u32::from(key.root) << 24)
        ^ (u32::from(key.level) << 4)
        ^ u32::from(key.underdraw);
    hash ^= hash >> 16;
    hash ^= hash >> 7;
    hash as usize
}

#[cfg(feature = "renderer-subdivision-cache")]
#[inline(always)]
fn resident_subdivision_directory_find(
    directory: &[u16],
    slots: &[ResidentSubdivisionSlot],
    key: ResidentSubdivisionKey,
) -> Option<usize> {
    debug_assert!(directory.len().is_power_of_two());
    let mask = directory.len() - 1;
    let mut bucket = resident_subdivision_key_hash(key) & mask;
    let mut probes = 0usize;
    while probes < directory.len() {
        let entry = directory[bucket];
        if entry == RESIDENT_SUBDIVISION_DIRECTORY_EMPTY {
            return None;
        }
        if entry != RESIDENT_SUBDIVISION_DIRECTORY_TOMBSTONE {
            let index = entry as usize;
            if slots[index].valid && slots[index].key == key {
                return Some(index);
            }
        }
        bucket = (bucket + 1) & mask;
        probes += 1;
    }
    None
}

#[cfg(feature = "renderer-subdivision-cache")]
#[inline]
fn resident_subdivision_directory_remove(
    directory: &mut [u16],
    slots: &[ResidentSubdivisionSlot],
    key: ResidentSubdivisionKey,
    slot_index: usize,
) {
    debug_assert!(directory.len().is_power_of_two());
    let mask = directory.len() - 1;
    let mut bucket = resident_subdivision_key_hash(key) & mask;
    let mut probes = 0usize;
    while probes < directory.len() {
        let entry = directory[bucket];
        if entry == RESIDENT_SUBDIVISION_DIRECTORY_EMPTY {
            return;
        }
        if entry != RESIDENT_SUBDIVISION_DIRECTORY_TOMBSTONE
            && entry as usize == slot_index
            && slots[slot_index].key == key
        {
            directory[bucket] = RESIDENT_SUBDIVISION_DIRECTORY_TOMBSTONE;
            return;
        }
        bucket = (bucket + 1) & mask;
        probes += 1;
    }
}

#[cfg(feature = "renderer-subdivision-cache")]
#[inline]
fn resident_subdivision_directory_insert(
    directory: &mut [u16],
    key: ResidentSubdivisionKey,
    slot_index: usize,
) {
    debug_assert!(directory.len().is_power_of_two());
    debug_assert!(slot_index < RESIDENT_SUBDIVISION_DIRECTORY_TOMBSTONE as usize);
    let mask = directory.len() - 1;
    let mut bucket = resident_subdivision_key_hash(key) & mask;
    let mut first_tombstone = None;
    let mut probes = 0usize;
    while probes < directory.len() {
        match directory[bucket] {
            RESIDENT_SUBDIVISION_DIRECTORY_EMPTY => {
                directory[first_tombstone.unwrap_or(bucket)] = slot_index as u16;
                return;
            }
            RESIDENT_SUBDIVISION_DIRECTORY_TOMBSTONE => {
                if first_tombstone.is_none() {
                    first_tombstone = Some(bucket);
                }
            }
            _ => {}
        }
        bucket = (bucket + 1) & mask;
        probes += 1;
    }
    let bucket = first_tombstone.expect("directory load factor is bounded below one");
    directory[bucket] = slot_index as u16;
}

#[cfg(feature = "renderer-subdivision-cache")]
struct ResidentSubdivisionSink<'a> {
    cache: &'a mut ResidentSubdivisionCache,
    pending_start: &'a mut *mut u32,
    active_pool: usize,
    frame: u32,
    hits: u32,
    allocations: u32,
    replacements: u32,
    fallbacks: u32,
    initializations: u32,
    packets: u32,
}

#[cfg(feature = "renderer-subdivision-cache")]
impl ClassicAffineSubdivisionCacheSink for ResidentSubdivisionSink<'_> {
    #[cfg(feature = "renderer-resident-base-cache")]
    #[inline(always)]
    fn acquire_base_packet(
        &mut self,
        source_face: u16,
        root: u8,
        quad: bool,
        material: u32,
        screen_z: u16,
    ) -> Option<ClassicAffineSubdivisionRootSlot> {
        let acquired = self.cache.acquire_base_packet(
            self.active_pool,
            self.frame,
            ResidentSubdivisionKey {
                material,
                source_face,
                root,
                level: u8::from(quad),
                underdraw: 0,
            },
            screen_z,
        );
        let Some(acquired) = acquired else {
            self.fallbacks = self.fallbacks.wrapping_add(1);
            return None;
        };
        self.hits = self.hits.wrapping_add(u32::from(acquired.logical_hit));
        self.allocations = self
            .allocations
            .wrapping_add(u32::from(!acquired.logical_hit));
        self.replacements = self
            .replacements
            .wrapping_add(u32::from(acquired.replaced));
        self.initializations = self
            .initializations
            .wrapping_add(u32::from(!acquired.resident));
        let pool_base = unsafe {
            addr_of_mut!(GPU_ARENAS)
                .cast::<u32>()
                .add(self.active_pool * GPU_ARENA_WORDS + DYNAMIC_GPU_ARENA_WORDS)
        };
        Some(ClassicAffineSubdivisionRootSlot {
            active: unsafe { pool_base.add(acquired.class_offset_words) },
            resident: acquired.resident,
        })
    }

    #[cfg_attr(feature = "renderer-resident-level2-cold-cache", inline(never))]
    fn acquire_root(
        &mut self,
        source_face: u16,
        root: u8,
        level: u8,
        underdraw: bool,
        material: u32,
        screen_z: u16,
    ) -> Option<ClassicAffineSubdivisionRootSlot> {
        let acquired = self.cache.acquire(
            self.active_pool,
            self.frame,
            ResidentSubdivisionKey {
                material,
                source_face,
                root,
                level,
                underdraw: u8::from(underdraw),
            },
            screen_z,
        );
        let Some(acquired) = acquired else {
            self.fallbacks = self.fallbacks.wrapping_add(1);
            return None;
        };
        self.hits = self.hits.wrapping_add(u32::from(acquired.logical_hit));
        self.allocations = self
            .allocations
            .wrapping_add(u32::from(!acquired.logical_hit));
        self.replacements = self
            .replacements
            .wrapping_add(u32::from(acquired.replaced));
        self.initializations = self
            .initializations
            .wrapping_add(u32::from(!acquired.resident));
        let pool_base = unsafe {
            addr_of_mut!(GPU_ARENAS)
                .cast::<u32>()
                .add(self.active_pool * GPU_ARENA_WORDS + DYNAMIC_GPU_ARENA_WORDS)
        };
        Some(ClassicAffineSubdivisionRootSlot {
            active: unsafe { pool_base.add(acquired.class_offset_words) },
            resident: acquired.resident,
        })
    }

    unsafe fn flush_dynamic_until(&mut self, end: *mut u32) {
        unsafe { crate::platform::gpu_insert_world_stream(*self.pending_start, end) };
        *self.pending_start = end;
    }

    unsafe fn insert_resident_packet(&mut self, packet: *mut u32, otz: u16, words: u8) {
        unsafe { crate::platform::gpu_insert_resident_world_packet(packet, otz, words) };
        self.packets = self.packets.wrapping_add(1);
    }

    unsafe fn insert_resident_stream(&mut self, first: *mut u32, end: *mut u32) {
        unsafe { crate::platform::gpu_insert_resident_world_stream(first, end) };
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct WaterPortal {
    plane: i16,
    leaf: u16,
}

/// `R_AddDynamicLights`' own term, `rad - dist`, in light bytes.
///
/// The distance is the octagonal approximation the particle pool already
/// trusts (longest axis plus a third of the other two, within a few percent),
/// because this runs once per corner of every face a light touches and a
/// square root there is not affordable.
#[inline(always)]
fn dynamic_light_add(light: DynamicLight, x: i32, y: i32, z: i32) -> i32 {
    let dx = (x - i32::from(light.origin.x)).abs();
    let dy = (y - i32::from(light.origin.y)).abs();
    let dz = (z - i32::from(light.origin.z)).abs();
    let longest = dx.max(dy).max(dz);
    light.radius_units() - (longest + (dx + dy + dz - longest) / 3)
}

/// True when a light cannot reach a box. All that survives of `R_MarkLights`
/// once there is no lightmap to walk the BSP for.
#[inline(always)]
fn dynamic_light_misses(light: DynamicLight, mins: [i16; 3], maxs: [i16; 3]) -> bool {
    let radius = light.radius_units();
    let origin = [light.origin.x, light.origin.y, light.origin.z];
    for axis in 0..3 {
        let origin = i32::from(origin[axis]);
        if i32::from(mins[axis]) - origin > radius || origin - i32::from(maxs[axis]) > radius {
            return true;
        }
    }
    false
}

#[cfg(feature = "renderer-topology-cache")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct TopologyBatchIdentity {
    hash_a: u32,
    hash_b: u32,
}

#[cfg(feature = "renderer-topology-cache")]
impl TopologyBatchIdentity {
    const fn new(map_generation: u32) -> Self {
        Self {
            hash_a: 0x811c_9dc5 ^ map_generation,
            hash_b: 0x9e37_79b9 ^ map_generation.rotate_left(16),
        }
    }

    fn mix(&mut self, source_surface: u16, texture_page: u16, clut: u16, stable: bool) {
        let geometry_material = u32::from(source_surface) | (u32::from(texture_page) << 16);
        let palette_policy = u32::from(clut) | (u32::from(stable) << 31);
        self.hash_a = (self.hash_a ^ geometry_material).wrapping_mul(0x0100_0193);
        self.hash_a = (self.hash_a ^ palette_policy).wrapping_mul(0x0100_0193);
        self.hash_b = self
            .hash_b
            .rotate_left(7)
            .wrapping_add(geometry_material.wrapping_mul(0x85eb_ca6b));
        self.hash_b = self
            .hash_b
            .rotate_left(7)
            .wrapping_add(palette_policy.wrapping_mul(0xc2b2_ae35));
    }
}

#[cfg(feature = "renderer-topology-cache")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct TopologyBatchCache {
    identity: TopologyBatchIdentity,
    plan: ClassicAffinePacketPlan,
    output_word_offset: u32,
    valid: bool,
}

#[cfg(feature = "renderer-static-world-reuse")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct StaticWorldKey {
    camera: Camera,
    visibility: Option<(u32, usize, u16)>,
    water_plane: i16,
}

#[cfg(feature = "renderer-static-world-reuse")]
struct StaticWorldCache {
    key: Option<StaticWorldKey>,
    world_words: u16,
    packets: u32,
    hardware_triangles: u32,
    visible_faces: u16,
    layered_sky_texture: Option<TextureInfo>,
    tag_slots: Vec<u16>,
}

#[cfg(feature = "renderer-static-world-reuse")]
impl StaticWorldCache {
    fn new() -> Self {
        Self {
            key: None,
            world_words: 0,
            packets: 0,
            hardware_triangles: 0,
            visible_faces: 0,
            layered_sky_texture: None,
            tag_slots: Vec::with_capacity(MAX_STATIC_WORLD_PACKET_SLOTS),
        }
    }

    fn invalidate(&mut self) {
        self.key = None;
        self.tag_slots.clear();
    }
}

/// Expand Quake's dominant baked indexed-corner stream without leaving the
/// retained materialization body. The fixed MIPS schedule preserves the
/// generic contract while avoiding a second hot-path call boundary.
///
/// # Safety
/// Every corner index must address `positions`; both source ranges and the
/// `vertex_count`-entry destination must be aligned, live, and non-overlapping.
#[cfg(feature = "renderer-quake-baked-materialize")]
#[inline(always)]
unsafe fn materialize_quake_baked_inline(
    corners: *const ClassicAffineIndexedCorner,
    positions: *const ClassicAffinePosition,
    position_count: usize,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
) {
    #[cfg(target_arch = "mips")]
    unsafe {
        let _ = position_count;
        core::arch::asm!(
            ".set noreorder",
            "beq   $6, $zero, 4f",
            "lui   $15, 0xffff",
            "2:",
            "lhu   $8, 0($4)",
            "lw    $9, 0($4)",
            "lw    $10, 4($4)",
            "sll   $11, $8, 1",
            "sll   $12, $8, 2",
            "addu  $11, $11, $12",
            "addu  $11, $5, $11",
            "lhu   $12, 0($11)",
            "lhu   $13, 2($11)",
            "lhu   $14, 4($11)",
            "sll   $13, $13, 16",
            "or    $12, $12, $13",
            "and   $9, $9, $15",
            "or    $9, $9, $14",
            "sw    $12, 0($7)",
            "sw    $9, 4($7)",
            "sw    $10, 8($7)",
            "addiu $4, $4, 8",
            "addiu $6, $6, -1",
            "bne   $6, $zero, 2b",
            "addiu $7, $7, 20",
            "4:",
            ".set reorder",
            inout("$4") corners => _,
            in("$5") positions,
            inout("$6") vertex_count => _,
            inout("$7") destination => _,
            lateout("$8") _,
            lateout("$9") _,
            lateout("$10") _,
            lateout("$11") _,
            lateout("$12") _,
            lateout("$13") _,
            lateout("$14") _,
            lateout("$15") _,
            options(nostack),
        );
    }

    #[cfg(not(target_arch = "mips"))]
    unsafe {
        materialize_classic_affine_indexed_baked_vertices(
            corners,
            positions,
            position_count,
            vertex_count,
            destination,
        );
    }
}

pub struct Renderer {
    arena: usize,
    frame: u32,
    // One byte per face, held in words so the rebuild can skip four
    // unmarked faces per load.
    face_visible: Vec<u32>,
    /// Map generation the `visible_faces` entries were decoded from.
    visible_faces_generation: Option<u32>,
    // Ascending faces decoded whenever the camera enters a different BSP
    // leaf. Iterating this list preserves legacy draw order while avoiding a
    // full face scan and repeated compact Face/Plane decoding every frame.
    visible_faces: Vec<VisibleFace>,
    /// Plane records are a cold parallel stream in the compact-cell path.
    /// Invariant-front faces never touch it during per-frame selection.
    #[cfg(feature = "renderer-compact-cell-stream")]
    visible_face_planes: Vec<CompactPlane>,
    #[cfg(feature = "renderer-block-frustum")]
    visible_face_blocks: Vec<VisibleFaceBlock>,
    #[cfg(feature = "renderer-hierarchical-block-frustum")]
    visible_face_super_blocks: Vec<VisibleFaceBlock>,
    // Indexes into `visible_faces` which survive the current camera frustum.
    // This keeps the projection/subdivision pass single-shot while the GTE is
    // loaded with the ordinary camera transform.
    frame_face_indices: Vec<u16>,
    visibility: [u8; MAX_VISIBILITY_BYTES],
    visible_leaf_count: usize,
    cached_visibility: Option<(u32, usize, u16)>,
    active_water_plane: i16,
    #[cfg(feature = "renderer-selection-cache")]
    cached_frame_selection: Option<(Camera, Option<(u32, usize, u16)>, i16)>,
    #[cfg(feature = "renderer-plane-index-cache")]
    plane_facing_generation: u32,
    #[cfg(feature = "renderer-plane-index-cache")]
    plane_facing_epoch: u16,
    #[cfg(feature = "renderer-plane-index-cache")]
    plane_facing_stamps: Vec<u16>,
    #[cfg(feature = "renderer-plane-index-cache")]
    plane_facing_behind: Vec<u8>,
    alias_projected: Vec<ClassicAliasProjectedVertex>,
    visible_entity_indices: Vec<u16>,
    cached_frustum: Option<(Camera, [AabbClipPlane; 4])>,
    light_styles: [u16; DUMMY_LIGHT_STYLE + 1],
    frame_light: Option<DynamicLight>,
    view_model_bob_phase: u16,
    view_model_light: u8,
    menu_packets: [Vec<QuadTexturedMaterial>; 2],
    hud_packets: [Vec<QuadTexturedMaterial>; 2],
    crosshair_packets: [[RectFlat; 4]; 2],
    centerprint_packets: [Vec<QuadTexturedMaterial>; 2],
    screen_tints: [[ScreenTintQuad; SCREEN_TINT_CAPACITY]; 2],
    liquid_generation: u32,
    liquid_phase: u8,
    liquid_uploaded_mask: u8,
    liquid_alternate_mask: u8,
    /// Which resident strip slots currently carry exact `inv2_*` pixels. At
    /// most one bit is set; every other slot carries its exact `inv_*` image.
    weapon_selected_mask: u8,
    active_textures: Vec<TextureInfo>,
    #[cfg(feature = "renderer-indexed-projection")]
    indexed_projection_generation: u32,
    #[cfg(feature = "renderer-indexed-projection")]
    indexed_position_slots: Vec<u8>,
    #[cfg(feature = "renderer-indexed-projection")]
    indexed_unique_positions: [u16; BATCH_MAX_VERTICES],
    #[cfg(feature = "renderer-indexed-projection")]
    indexed_corner_slots: [u8; BATCH_MAX_VERTICES],
    #[cfg(feature = "renderer-indexed-projection")]
    indexed_projected: [ClassicAliasProjectedVertex; BATCH_MAX_VERTICES],
    #[cfg(feature = "renderer-indexed-projection")]
    indexed_unique_count: usize,
    #[cfg(feature = "renderer-topology-cache")]
    topology_batches: [[TopologyBatchCache; MAX_TOPOLOGY_CACHE_BATCHES]; 2],
    #[cfg(feature = "renderer-census")]
    subdivision_slab_cache_models: [SubdivisionSlabCacheModel; SUBDIVISION_CACHE_BUDGETS_KIB.len()],
    #[cfg(feature = "renderer-subdivision-cache")]
    resident_subdivision_cache: ResidentSubdivisionCache,
    #[cfg(feature = "renderer-static-world-reuse")]
    static_world_cache: [StaticWorldCache; 2],
}

impl Renderer {
    pub fn new() -> Self {
        let light_styles = quake_core::lightstyle::initial_values();
        Self {
            arena: 0,
            frame: 0,
            face_visible: vec![0; MAX_FACE_COUNT.div_ceil(4)],
            visible_faces_generation: None,
            visible_faces: Vec::with_capacity(MAX_VISIBLE_FACE_COUNT),
            #[cfg(feature = "renderer-compact-cell-stream")]
            visible_face_planes: Vec::with_capacity(MAX_VISIBLE_FACE_COUNT),
            #[cfg(feature = "renderer-block-frustum")]
            visible_face_blocks: Vec::with_capacity(MAX_VISIBLE_FACE_BLOCKS),
            #[cfg(feature = "renderer-hierarchical-block-frustum")]
            visible_face_super_blocks: Vec::with_capacity(MAX_VISIBLE_FACE_SUPER_BLOCKS),
            frame_face_indices: Vec::with_capacity(MAX_VISIBLE_FACE_COUNT),
            visibility: [0; MAX_VISIBILITY_BYTES],
            visible_leaf_count: 0,
            cached_visibility: None,
            active_water_plane: -1,
            #[cfg(feature = "renderer-selection-cache")]
            cached_frame_selection: None,
            #[cfg(feature = "renderer-plane-index-cache")]
            plane_facing_generation: u32::MAX,
            #[cfg(feature = "renderer-plane-index-cache")]
            plane_facing_epoch: 0,
            #[cfg(feature = "renderer-plane-index-cache")]
            plane_facing_stamps: Vec::new(),
            #[cfg(feature = "renderer-plane-index-cache")]
            plane_facing_behind: Vec::new(),
            alias_projected: vec![ClassicAliasProjectedVertex::default(); MAX_ALIAS_VERTICES],
            visible_entity_indices: Vec::with_capacity(MAX_RENDER_ENTITIES),
            cached_frustum: None,
            light_styles,
            frame_light: None,
            view_model_bob_phase: 0,
            view_model_light: 0,
            menu_packets: core::array::from_fn(|_| Vec::with_capacity(MENU_PACKET_CAPACITY)),
            hud_packets: core::array::from_fn(|_| Vec::with_capacity(HUD_PACKET_CAPACITY)),
            crosshair_packets: [crosshair_packets(), crosshair_packets()],
            centerprint_packets: core::array::from_fn(|_| {
                Vec::with_capacity(CENTERPRINT_PACKET_CAPACITY)
            }),
            screen_tints: [[ScreenTintQuad::new((0, 0, 0), BlendMode::Add); SCREEN_TINT_CAPACITY];
                2],
            liquid_generation: u32::MAX,
            liquid_phase: u8::MAX,
            liquid_uploaded_mask: 0,
            liquid_alternate_mask: 0,
            weapon_selected_mask: 0,
            active_textures: Vec::with_capacity(MAX_RENDER_TEXTURES),
            #[cfg(feature = "renderer-indexed-projection")]
            indexed_projection_generation: u32::MAX,
            #[cfg(feature = "renderer-indexed-projection")]
            indexed_position_slots: Vec::new(),
            #[cfg(feature = "renderer-indexed-projection")]
            indexed_unique_positions: [0; BATCH_MAX_VERTICES],
            #[cfg(feature = "renderer-indexed-projection")]
            indexed_corner_slots: [0; BATCH_MAX_VERTICES],
            #[cfg(feature = "renderer-indexed-projection")]
            indexed_projected: [ClassicAliasProjectedVertex::default(); BATCH_MAX_VERTICES],
            #[cfg(feature = "renderer-indexed-projection")]
            indexed_unique_count: 0,
            #[cfg(feature = "renderer-topology-cache")]
            topology_batches: [[TopologyBatchCache::default(); MAX_TOPOLOGY_CACHE_BATCHES]; 2],
            #[cfg(feature = "renderer-census")]
            subdivision_slab_cache_models: core::array::from_fn(|index| {
                SubdivisionSlabCacheModel::new(SUBDIVISION_CACHE_BUDGETS_KIB[index] * 1024)
            }),
            #[cfg(feature = "renderer-subdivision-cache")]
            resident_subdivision_cache: ResidentSubdivisionCache::new(),
            #[cfg(feature = "renderer-static-world-reuse")]
            static_world_cache: core::array::from_fn(|_| StaticWorldCache::new()),
        }
    }

    #[cfg(feature = "renderer-plane-index-cache")]
    fn prepare_plane_facing_cache(&mut self, map: &ResidentMap) {
        let plane_count = map.collision_planes().len();
        if self.plane_facing_generation != map.generation()
            || self.plane_facing_stamps.len() != plane_count
        {
            self.plane_facing_stamps.clear();
            self.plane_facing_stamps.resize(plane_count, 0);
            self.plane_facing_behind.clear();
            self.plane_facing_behind.resize(plane_count, 0);
            self.plane_facing_generation = map.generation();
            self.plane_facing_epoch = 0;
        }
        self.plane_facing_epoch = self.plane_facing_epoch.wrapping_add(1);
        if self.plane_facing_epoch == 0 {
            self.plane_facing_stamps.fill(0);
            self.plane_facing_epoch = 1;
        }
    }

    #[cfg(feature = "renderer-indexed-projection")]
    fn prepare_indexed_world_projection(&mut self, map: &ResidentMap) {
        let indexed = map.indexed_vertices().expect("validated PSB4 vertices");
        if self.indexed_projection_generation != map.generation()
            || self.indexed_position_slots.len() != indexed.positions.len()
        {
            self.indexed_position_slots.clear();
            self.indexed_position_slots
                .resize(indexed.positions.len(), u8::MAX);
            self.indexed_projection_generation = map.generation();
        }
        debug_assert_eq!(self.indexed_unique_count, 0);
    }

    #[cfg(feature = "renderer-subdivision-cache")]
    unsafe fn flush_cached_subdivision_batch(
        &mut self,
        vertices: *mut ClassicAffineVertex,
        vertex_count: usize,
        surfaces: *const ClassicAffineBatchSurface,
        source_faces: *const u16,
        surface_count: usize,
        output: *mut u32,
        pending_start: &mut *mut u32,
        stats: &mut RenderStats,
    ) -> ClassicAffineSubmit {
        let mut sink = ResidentSubdivisionSink {
            cache: &mut self.resident_subdivision_cache,
            pending_start,
            active_pool: self.arena,
            frame: self.frame,
            hits: 0,
            allocations: 0,
            replacements: 0,
            fallbacks: 0,
            initializations: 0,
            packets: 0,
        };
        let submitted = unsafe {
            submit_classic_affine_cached_subdivision_batch(
                vertices,
                vertex_count,
                surfaces,
                source_faces,
                surface_count,
                output,
                ClassicAffineProfile::QUAKE_REFERENCE,
                &mut sink,
            )
        };
        stats.subdivision_cache_hits = stats.subdivision_cache_hits.wrapping_add(sink.hits);
        stats.subdivision_cache_allocations = stats
            .subdivision_cache_allocations
            .wrapping_add(sink.allocations);
        stats.subdivision_cache_replacements = stats
            .subdivision_cache_replacements
            .wrapping_add(sink.replacements);
        stats.subdivision_cache_fallbacks = stats
            .subdivision_cache_fallbacks
            .wrapping_add(sink.fallbacks);
        stats.subdivision_cache_initializations = stats
            .subdivision_cache_initializations
            .wrapping_add(sink.initializations);
        stats.subdivision_cache_packets = stats
            .subdivision_cache_packets
            .wrapping_add(sink.packets);
        submitted
    }

    #[cfg(feature = "renderer-fused-materialize-project")]
    #[allow(clippy::too_many_arguments)]
    unsafe fn flush_fused_world_batch(
        &self,
        map: &ResidentMap,
        vertices: *mut ClassicAffineVertex,
        vertex_count: usize,
        surfaces: *const ClassicAffineBatchSurface,
        sources: *const ClassicAffineIndexedBatchSource,
        visible_indices: *const u16,
        surface_count: usize,
        output: *mut u32,
    ) -> ClassicAffineSubmit {
        if vertex_count == 0 || surface_count == 0 {
            return ClassicAffineSubmit {
                next_packet: output,
                packets: 0,
                hardware_triangles: 0,
            };
        }
        let indexed = map.indexed_vertices().expect("validated PSB4 vertices");
        unsafe {
            materialize_project_classic_affine_indexed_batch(
                indexed
                    .corners
                    .as_ptr()
                    .cast::<ClassicAffineIndexedCorner>(),
                indexed.positions.as_ptr().cast::<ClassicAffinePosition>(),
                indexed.positions.len(),
                surfaces,
                sources,
                surface_count,
                vertex_count,
                vertices,
            );
        }
        if self.frame_light.is_some() {
            let mut surface_index = 0usize;
            while surface_index < surface_count {
                let surface = unsafe { ptr::read(surfaces.add(surface_index)) };
                let visible_index = unsafe { ptr::read(visible_indices.add(surface_index)) };
                let face_vertices = unsafe {
                    core::slice::from_raw_parts_mut(
                        vertices.add(surface.first_vertex as usize),
                        surface.vertex_count as usize,
                    )
                };
                self.light_face(visible_index as usize, face_vertices);
                surface_index += 1;
            }
        }
        unsafe {
            submit_classic_affine_projected_batch(
                vertices,
                vertex_count,
                surfaces,
                surface_count,
                output,
                ClassicAffineProfile::QUAKE_REFERENCE,
            )
        }
    }

    #[cfg(feature = "renderer-indexed-projection")]
    unsafe fn flush_indexed_world_batch(
        &mut self,
        map: &ResidentMap,
        vertices: *mut ClassicAffineVertex,
        vertex_count: usize,
        surfaces: *const ClassicAffineBatchSurface,
        surface_count: usize,
        output: *mut u32,
        stats: &mut RenderStats,
    ) -> ClassicAffineSubmit {
        if vertex_count == 0 || surface_count == 0 {
            debug_assert_eq!(self.indexed_unique_count, 0);
            return ClassicAffineSubmit {
                next_packet: output,
                packets: 0,
                hardware_triangles: 0,
            };
        }

        let indexed = map.indexed_vertices().expect("validated PSB4 vertices");
        unsafe {
            project_classic_affine_indexed_vertices_dense(
                indexed.positions.as_ptr().cast::<ClassicAffinePosition>(),
                indexed.positions.len(),
                self.indexed_unique_positions.as_ptr(),
                self.indexed_unique_count,
                self.indexed_projected.as_mut_ptr(),
            );
        }
        let mut corner = 0usize;
        while corner < vertex_count {
            let slot = unsafe { *self.indexed_corner_slots.get_unchecked(corner) } as usize;
            let projected = unsafe { *self.indexed_projected.get_unchecked(slot) };
            unsafe {
                (*vertices.add(corner)).screen = projected.screen;
                (*vertices.add(corner)).depth = i32::from(projected.depth);
            }
            corner += 1;
        }
        let submitted = unsafe {
            submit_classic_affine_projected_batch(
                vertices,
                vertex_count,
                surfaces,
                surface_count,
                output,
                ClassicAffineProfile::QUAKE_REFERENCE,
            )
        };

        stats.indexed_projection_corners = stats
            .indexed_projection_corners
            .wrapping_add(vertex_count as u32);
        stats.indexed_projection_unique = stats
            .indexed_projection_unique
            .wrapping_add(self.indexed_unique_count as u32);
        let mut index = 0usize;
        while index < self.indexed_unique_count {
            let position = unsafe { *self.indexed_unique_positions.get_unchecked(index) } as usize;
            unsafe { *self.indexed_position_slots.get_unchecked_mut(position) = u8::MAX };
            index += 1;
        }
        self.indexed_unique_count = 0;
        submitted
    }

    #[cfg(feature = "renderer-topology-cache")]
    unsafe fn flush_resident_world_batch(
        &mut self,
        vertices: *mut ClassicAffineVertex,
        vertex_count: usize,
        surfaces: *const ClassicAffineResidentBatchSurface,
        surface_count: usize,
        arena_start: *mut u32,
        output: *mut u32,
        batch_index: usize,
        identity: TopologyBatchIdentity,
    ) -> ClassicAffinePlannedSubmit {
        if vertex_count == 0 || surface_count == 0 {
            return unsafe {
                submit_classic_affine_planned_resident_batch(
                    vertices,
                    vertex_count,
                    surfaces,
                    surface_count,
                    output,
                    ClassicAffineProfile::QUAKE_REFERENCE,
                    None,
                )
            };
        }
        let output_word_offset = unsafe { output.offset_from(arena_start) as u32 };
        let expected = self
            .topology_batches
            .get(self.arena)
            .and_then(|pool| pool.get(batch_index))
            .filter(|cache| {
                cache.valid
                    && cache.identity == identity
                    && cache.output_word_offset == output_word_offset
            })
            .map(|cache| &cache.plan);
        let submitted = unsafe {
            submit_classic_affine_planned_resident_batch(
                vertices,
                vertex_count,
                surfaces,
                surface_count,
                output,
                ClassicAffineProfile::QUAKE_REFERENCE,
                expected,
            )
        };
        if let Some(cache) = self
            .topology_batches
            .get_mut(self.arena)
            .and_then(|pool| pool.get_mut(batch_index))
        {
            *cache = TopologyBatchCache {
                identity,
                plan: submitted.plan,
                output_word_offset,
                valid: submitted.plan.is_valid(),
            };
        }
        submitted
    }

    /// Adopt the gameplay layer's `d_lightstylevalue` for this frame.
    ///
    /// The table is owned by `EntityScene`, because `light_use` writes it and
    /// the entity relight reads it; the renderer only samples it per face.
    #[inline(never)]
    pub fn set_light_styles(&mut self, styles: &[u16; DUMMY_LIGHT_STYLE + 1]) {
        self.light_styles = *styles;
    }

    /// Adopt the gameplay layer's live `cl_dlights` for this frame.
    ///
    /// PORT NOTE, and it is a real difference: this is NOT `R_MarkLights` plus
    /// the original's dynamic lightmap patch. A PS1 port that pre-lights
    /// vertices and materializes faces from baked light has no lightmap to
    /// rebuild, and rebuilding one per frame is not affordable. What this does
    /// instead is add id1's own `radius - distance` term to the light the
    /// materializer already writes per face corner, and to the scalar
    /// `R_LightPoint` sample an alias model is tinted by. So: the falloff and
    /// the lifetimes are the original's, the resolution is a face corner
    /// rather than a luxel, a light is never occluded by the wall between it
    /// and the surface, and sky, liquid and brush-model (door, platform)
    /// faces are left alone.
    #[inline(never)]
    pub fn set_dynamic_lights(&mut self, lights: &quake_core::effects::DynamicLights) {
        self.frame_light = lights.active().next();
    }

    /// Add one live light to the corners of one materialized world face.
    ///
    /// Out of line, one light per call, and called behind a live-light test:
    /// the world loop is at the register limit, and both the loop over lights
    /// and this body inlined there cost image on the overwhelming majority of
    /// frames that have no light at all.
    #[inline(never)]
    fn light_face(&self, visible_index: usize, vertices: &mut [ClassicAffineVertex]) {
        let Some(light) = self.frame_light else {
            return;
        };
        let visible = unsafe { self.visible_faces.get_unchecked(visible_index) };
        if dynamic_light_misses(light, visible.bounds.mins, visible.bounds.maxs) {
            return;
        }
        for vertex in vertices.iter_mut() {
            let add = dynamic_light_add(
                light,
                i32::from(vertex.position[0]),
                i32::from(vertex.position[1]),
                i32::from(vertex.position[2]),
            );
            // Both the baked and the style-weighted materializer paths write
            // one grey level into all three channels, so one channel is the
            // light this has to lift.
            let lit = (((vertex.color & 0xff) as i32) + add.max(0)).min(255) as u32;
            vertex.color = lit | (lit << 8) | (lit << 16);
        }
    }

    /// `R_DrawAliasModel`'s own dlight loop: every live light adds
    /// `radius - distance` to a point's `R_LightPoint` sample.
    #[inline(never)]
    fn dynamic_light_at(&self, origin: Vec3I32) -> i32 {
        match self.frame_light {
            Some(light) => {
                dynamic_light_add(light, origin.x >> 12, origin.y >> 12, origin.z >> 12).max(0)
            }
            None => 0,
        }
    }

    /// Materialise only the liquid tiles which survive PVS, facing and
    /// frustum selection. The original immutable 64x64 sources remain in the
    /// resident map; a new phase overwrites their exact atlas rectangles.
    fn update_visible_liquid_tiles(&mut self, map: &ResidentMap, tick_60hz: u32) {
        let liquids = map.liquid_textures();
        if liquids.is_empty() {
            return;
        }
        let mut visible_mask = 0u8;
        for &frame_entry in &self.frame_face_indices {
            let visible_index = (frame_entry & FRAME_FACE_INDEX_MASK) as usize;
            let visible = &self.visible_faces[visible_index];
            #[cfg(feature = "renderer-cell-liquid-policy")]
            if visible.bounds.surface_index & VISIBLE_LIQUID_BIT == 0 {
                continue;
            }
            let texture_index = visible.face.material;
            for (liquid_index, liquid) in liquids.iter().enumerate() {
                if liquid.texture_index == texture_index {
                    visible_mask |= 1 << liquid_index;
                }
            }
        }
        if visible_mask == 0 {
            return;
        }

        let phase = quake_core::liquid::phase_from_tick(tick_60hz);
        if self.liquid_phase != phase {
            self.liquid_phase = phase;
            self.liquid_uploaded_mask = 0;
        }
        let missing_mask = visible_mask & !self.liquid_uploaded_mask;
        if missing_mask == 0 {
            return;
        }

        #[cfg(feature = "renderer-scratchpad-liquid-phase")]
        let phase_offsets = unsafe {
            core::slice::from_raw_parts_mut(
                psx_engine::scratchpad::base_ptr(),
                quake_core::liquid::LIQUID_PHASE_OFFSETS,
            )
        };
        #[cfg(feature = "renderer-scratchpad-liquid-phase")]
        if !quake_core::liquid::prepare_phase_offsets(phase, phase_offsets) {
            return;
        }

        const EMPTY_UPLOAD: crate::platform::VramUploadRange = crate::platform::VramUploadRange {
            rect: psx_vram::VramRect::new(0, 0, 1, 1),
            start: 0,
            len: 2,
        };
        let mut uploads = [EMPTY_UPLOAD; MAX_LIQUID_TEXTURES];
        let mut upload_count = 0usize;
        for (liquid_index, &liquid) in liquids.iter().enumerate() {
            if missing_mask & (1 << liquid_index) == 0 {
                continue;
            }
            let Some(source) = map.liquid_source(liquid) else {
                return;
            };
            let start = liquid_index * quake_core::liquid::LIQUID_TILE_BYTES;
            let destination = unsafe {
                core::slice::from_raw_parts_mut(
                    addr_of_mut!(LIQUID_WARP).cast::<u8>().add(start),
                    quake_core::liquid::LIQUID_TILE_BYTES,
                )
            };
            #[cfg(not(feature = "renderer-scratchpad-liquid-phase"))]
            let warped = quake_core::liquid::warp_tile_64(source, destination, phase);
            #[cfg(feature = "renderer-scratchpad-liquid-phase")]
            let warped =
                quake_core::liquid::warp_tile_64_prepared(source, destination, phase_offsets);
            if !warped {
                return;
            }
            let alternate_active = quake_core::liquid::alternate_tile_is_active(
                self.liquid_alternate_mask,
                liquid_index,
            );
            let destination = if alternate_active {
                liquid.primary
            } else {
                liquid.alternate
            };
            let Some(rect) = texture_rect(destination) else {
                return;
            };
            uploads[upload_count] = crate::platform::VramUploadRange {
                rect,
                start,
                len: quake_core::liquid::LIQUID_TILE_BYTES,
            };
            upload_count += 1;
        }
        let upload_bytes = unsafe {
            core::slice::from_raw_parts(addr_of_mut!(LIQUID_WARP).cast::<u8>(), LIQUID_WARP_BYTES)
        };
        // The warp buffer is static and rewritten only by a later frame's own
        // liquid pass, satisfying the staging lifetime `defer_vram_ranges`
        // requires. Validation happens here, so committing the flip now is
        // sound: the flush at end of frame cannot fail.
        let staged =
            unsafe { crate::platform::defer_vram_ranges(upload_bytes, &uploads[..upload_count]) };
        if staged.is_ok() {
            for (liquid_index, liquid) in liquids.iter().enumerate() {
                if missing_mask & (1 << liquid_index) == 0 {
                    continue;
                }
                self.liquid_alternate_mask = quake_core::liquid::commit_tile_upload(
                    self.liquid_alternate_mask,
                    liquid_index,
                );
                let texture = if quake_core::liquid::alternate_tile_is_active(
                    self.liquid_alternate_mask,
                    liquid_index,
                ) {
                    liquid.alternate
                } else {
                    liquid.primary
                };
                self.active_textures[liquid.texture_index as usize] = texture;
            }
            self.liquid_uploaded_mask |= missing_mask;
        }
    }

    pub fn draw_frame(
        &mut self,
        map: &ResidentMap,
        mut camera: Camera,
        animation_tick_60hz: u32,
        water_warp: bool,
        water_alpha: bool,
        entities: &[RenderEntity],
        lightning_beam: Option<LightningBeam>,
        explosion_effects: impl Iterator<Item = ExplosionEffect>,
        impact_particles: impl Iterator<Item = ImpactParticle>,
        rotating_yaw: i16,
        view_model: Option<ViewModelInput>,
        hud: Option<HudView>,
        centerprint: Option<&str>,
        menu: Option<MenuView>,
        intermission: Option<IntermissionView>,
        now_playing: Option<(&'static str, u32)>,
        screen_blend: &quake_core::screenblend::ScreenBlend,
    ) -> RenderStats {
        crate::platform::gpu_begin_frame();
        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::stage_begin(psx_telemetry::stage::RENDER);
        if matches!(unsafe { HUD_MODE }, HudMode::Classic) {
            if let Some(view) = hud {
                self.stage_weapon_icons(map, view);
            }
        }
        if water_warp {
            let warp = quake_core::waterwarp::sample(animation_tick_60hz);
            camera.angles[2] = camera.angles[2].wrapping_add(warp.roll);
            crate::platform::configure_underwater_projection(
                warp.offset_x,
                warp.offset_y,
                warp.projection_plane,
            );
        } else {
            crate::platform::configure_quake_projection();
        }
        if self.liquid_generation != map.generation() {
            self.active_textures.clear();
            self.active_textures
                .extend_from_slice(map.render_textures());
            self.liquid_generation = map.generation();
            self.liquid_phase = u8::MAX;
            self.liquid_uploaded_mask = 0;
            self.liquid_alternate_mask = 0;
        }
        #[cfg(feature = "renderer-subdivision-cache")]
        self.resident_subdivision_cache.begin_map(map.generation());
        let start = unsafe {
            addr_of_mut!(GPU_ARENAS)
                .cast::<u32>()
                .add(self.arena * GPU_ARENA_WORDS)
        };
        #[cfg(not(feature = "renderer-subdivision-cache"))]
        let end = unsafe { start.add(GPU_ARENA_WORDS) };
        #[cfg(feature = "renderer-subdivision-cache")]
        let end = unsafe { start.add(DYNAMIC_GPU_ARENA_WORDS) };
        let mut next = start;
        #[cfg(feature = "renderer-subdivision-cache")]
        let mut pending_world_start = start;
        let mut stats = RenderStats::default();
        let mut layered_sky_texture = None;

        let frustum = self.frustum(camera);
        // The MIPS AABB classifier consumes the four planes from the GTE
        // rotation registers; load them once before frame-face selection.
        #[cfg(all(
            not(feature = "renderer-selection-cache"),
            not(feature = "renderer-gte-near-classification")
        ))]
        scene::load_aabb_clip4(&frustum);
        #[cfg(feature = "renderer-census")]
        let previous_visibility = self.cached_visibility;
        #[cfg(feature = "renderer-census")]
        let mut renderer_census = {
            for model in &mut self.subdivision_slab_cache_models {
                model.begin_frame(map.generation());
            }
            RendererCensus::default()
        };
        let visibility_valid = self.prepare_visibility(map, camera, water_alpha);
        #[cfg(feature = "renderer-census")]
        {
            renderer_census.visibility_rebuilt =
                u32::from(previous_visibility != self.cached_visibility);
            if let Some((_, leaf, portal_leaf)) = self.cached_visibility {
                renderer_census.leaf = leaf as u32;
                renderer_census.portal_leaf = u32::from(portal_leaf);
            } else {
                renderer_census.leaf = u32::MAX;
                renderer_census.portal_leaf = u32::MAX;
            }
        }
        #[cfg(all(feature = "renderer-selection-cache", not(feature = "renderer-census")))]
        let selection_cached = visibility_valid
            && self.cached_frame_selection
                == Some((camera, self.cached_visibility, self.active_water_plane));
        #[cfg(any(not(feature = "renderer-selection-cache"), feature = "renderer-census"))]
        let selection_cached = false;
        #[cfg(feature = "renderer-gte-near-classification")]
        let mut near_clip_plane = None;
        if !selection_cached {
            #[cfg(all(
                feature = "renderer-selection-cache",
                not(feature = "renderer-gte-near-classification")
            ))]
            scene::load_aabb_clip4(&frustum);
            #[cfg(feature = "renderer-gte-near-classification")]
            {
                near_clip_plane = Some(NearPlane::new(camera).as_aabb_clip_plane());
                load_aabb_clip4_with_near(
                    &frustum,
                    near_clip_plane.as_ref().expect("near plane initialized"),
                );
            }
            #[cfg(feature = "renderer-plane-index-cache")]
            self.prepare_plane_facing_cache(map);
            self.frame_face_indices.clear();
        }
        if visibility_valid && !selection_cached {
            #[cfg(all(
                not(feature = "renderer-census"),
                not(feature = "renderer-aabb-support-offsets"),
                not(feature = "renderer-block-frustum")
            ))]
            select_frame_faces(
                &self.visible_faces,
                &self.active_textures,
                camera.origin,
                &frustum,
                self.active_water_plane,
                &mut self.frame_face_indices,
            );
            #[cfg(all(
                not(feature = "renderer-census"),
                feature = "renderer-aabb-support-offsets",
                not(feature = "renderer-block-frustum")
            ))]
            {
                let supports = scene::AabbClip4SupportOffsets::new(&frustum);
                select_frame_faces_preselected(
                    &self.visible_faces,
                    &self.active_textures,
                    camera.origin,
                    &frustum,
                    &supports,
                    self.active_water_plane,
                    &mut self.frame_face_indices,
                );
            }
            #[cfg(all(
                not(feature = "renderer-census"),
                feature = "renderer-block-frustum",
                not(feature = "renderer-hierarchical-block-frustum"),
                not(feature = "renderer-plane-index-cache")
            ))]
            select_frame_faces_blocked(
                &self.visible_faces,
                #[cfg(feature = "renderer-compact-cell-stream")]
                &self.visible_face_planes,
                &self.visible_face_blocks,
                #[cfg(not(feature = "renderer-cell-liquid-policy"))]
                &self.active_textures,
                camera.origin,
                &frustum,
                self.active_water_plane,
                &mut self.frame_face_indices,
            );
            #[cfg(all(
                not(feature = "renderer-census"),
                feature = "renderer-block-frustum",
                not(feature = "renderer-hierarchical-block-frustum"),
                feature = "renderer-plane-index-cache"
            ))]
            select_frame_faces_blocked_plane_indexed(
                &self.visible_faces,
                &self.visible_face_blocks,
                &self.active_textures,
                camera.origin,
                &frustum,
                self.active_water_plane,
                self.plane_facing_epoch,
                &mut self.plane_facing_stamps,
                &mut self.plane_facing_behind,
                &mut self.frame_face_indices,
            );
            #[cfg(all(
                not(feature = "renderer-census"),
                feature = "renderer-hierarchical-block-frustum"
            ))]
            select_frame_faces_hierarchical(
                &self.visible_faces,
                &self.visible_face_blocks,
                &self.visible_face_super_blocks,
                &self.active_textures,
                camera.origin,
                &frustum,
                self.active_water_plane,
                &mut self.frame_face_indices,
            );
            #[cfg(feature = "renderer-census")]
            {
                renderer_census.selection = select_frame_faces_census(
                    &self.visible_faces,
                    &self.active_textures,
                    camera.origin,
                    &frustum,
                    self.active_water_plane,
                    &mut self.frame_face_indices,
                );
            }
            #[cfg(not(feature = "renderer-gte-near-classification"))]
            flag_near_faces(
                &self.visible_faces,
                &mut self.frame_face_indices,
                NearPlane::new(camera),
            );
            #[cfg(feature = "renderer-gte-near-classification")]
            flag_near_faces_gte(
                &self.visible_faces,
                &mut self.frame_face_indices,
                near_clip_plane.as_ref().expect("near plane initialized"),
            );
        }
        #[cfg(all(feature = "renderer-selection-cache", not(feature = "renderer-census")))]
        {
            self.cached_frame_selection = if visibility_valid {
                Some((camera, self.cached_visibility, self.active_water_plane))
            } else {
                None
            };
        }
        if visibility_valid {
            #[cfg(feature = "renderer-census")]
            {
                renderer_census.near_faces = self
                    .frame_face_indices
                    .iter()
                    .filter(|&&entry| entry & NEAR_FACE_BIT != 0)
                    .count() as u32;
                renderer_census.blocks = census_face_blocks(
                    &self.visible_faces,
                    &self.active_textures,
                    camera.origin,
                    &frustum,
                    self.active_water_plane,
                );
                renderer_census.projection = census_projection_batches(
                    map,
                    &self.visible_faces,
                    &self.active_textures,
                    &self.frame_face_indices,
                    self.frame_light,
                );
                (
                    renderer_census.selected_hash_a,
                    renderer_census.selected_hash_b,
                ) = selected_fingerprints(&self.frame_face_indices);
            }
        }
        let view = crate::platform::load_quake_camera(
            [camera.origin.x, camera.origin.y, camera.origin.z],
            camera.angles,
        );
        self.update_visible_liquid_tiles(map, animation_tick_60hz);
        #[cfg(feature = "renderer-static-world-reuse")]
        let static_world_key = StaticWorldKey {
            camera,
            visibility: self.cached_visibility,
            water_plane: self.active_water_plane,
        };
        #[cfg(feature = "renderer-static-world-reuse")]
        let static_world_cache_hit = if visibility_valid && self.frame_light.is_none() {
            let cache = unsafe { self.static_world_cache.get_unchecked(self.arena) };
            if cache.key == Some(static_world_key) {
                let cached_end = unsafe { start.add(cache.world_words as usize) };
                if unsafe { restore_static_world_tags(start, cached_end, &cache.tag_slots) } {
                    next = cached_end;
                    stats.visible_faces = cache.visible_faces;
                    stats.packets = cache.packets;
                    stats.hardware_triangles = cache.hardware_triangles;
                    layered_sky_texture = cache.layered_sky_texture;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        #[cfg(not(feature = "renderer-static-world-reuse"))]
        let static_world_cache_hit = false;
        #[cfg(feature = "renderer-static-world-reuse")]
        let mut static_world_cache_eligible = self.frame_light.is_none();

        if visibility_valid && !static_world_cache_hit {
            #[cfg(feature = "renderer-hoisted-indexed-world")]
            let indexed_world = map.indexed_vertices().expect("validated PSB4 vertices");
            #[cfg(feature = "renderer-indexed-projection")]
            self.prepare_indexed_world_projection(map);
            let batch_vertices = scratchpad_batch_vertices();
            #[cfg(not(feature = "renderer-topology-cache"))]
            let mut batch_surfaces = uninit_batch_surfaces();
            #[cfg(feature = "renderer-fused-materialize-project")]
            let mut batch_indexed_sources = uninit_batch_indexed_sources();
            #[cfg(feature = "renderer-fused-materialize-project")]
            let mut batch_visible_indices = uninit_batch_visible_indices();
            #[cfg(any(feature = "renderer-census", feature = "renderer-subdivision-cache"))]
            let mut batch_source_surfaces = uninit_batch_source_surfaces();
            #[cfg(feature = "renderer-topology-cache")]
            let mut batch_surfaces = uninit_resident_batch_surfaces();
            let mut batch_vertex_count = 0usize;
            let mut batch_surface_count = 0usize;
            let mut batch_worst_words = 0usize;
            #[cfg(feature = "renderer-topology-cache")]
            let mut topology_batch_index = 0usize;
            #[cfg(feature = "renderer-topology-cache")]
            let mut topology_batch_identity = TopologyBatchIdentity::new(map.generation());

            for frame_index in 0..self.frame_face_indices.len() {
                // Copy before mutably borrowing `self` in the submission path.
                let frame_entry = unsafe { *self.frame_face_indices.get_unchecked(frame_index) };
                let visible_index = (frame_entry & FRAME_FACE_INDEX_MASK) as usize;
                let near = frame_entry & NEAR_FACE_BIT != 0;
                let water_blend = frame_entry & WATER_BLEND_FACE_BIT != 0;
                // Keep this local by value: resident subdivision submission
                // mutably borrows the renderer while the face metadata is
                // still needed to form the cache key below.
                let visible = unsafe { *self.visible_faces.get_unchecked(visible_index) };
                let face = visible.face;
                #[cfg(feature = "renderer-topology-cache")]
                let visible_bounds = visible.bounds;
                let texture =
                    unsafe { *self.active_textures.get_unchecked(face.material as usize) };

                let vertex_count = face.corner_count as usize;
                // A near clip can add one vertex; reserve for it up front.
                let clip = near && vertex_count < NEAR_CLIP_MAX_VERTICES;
                let reserve_count = vertex_count + usize::from(clip);
                if texture.flags & TEXTURE_LAYERED_SKY != 0 {
                    if let Some(selected) = layered_sky_texture {
                        debug_assert_eq!(selected, texture);
                    } else {
                        layered_sky_texture = Some(texture);
                    }
                    stats.visible_faces = stats.visible_faces.saturating_add(1);
                    continue;
                }
                if texture.flags & (TEXTURE_LIQUID | TEXTURE_SKY) != 0 {
                    #[cfg(feature = "renderer-static-world-reuse")]
                    {
                        static_world_cache_eligible = false;
                    }
                    #[cfg(all(
                        feature = "renderer-census",
                        not(feature = "renderer-topology-cache")
                    ))]
                    let batch_output = next;
                    #[cfg(all(
                        not(feature = "renderer-topology-cache"),
                        not(feature = "renderer-indexed-projection"),
                        not(feature = "renderer-subdivision-cache")
                    ))]
                    let submitted = unsafe {
                        #[cfg(feature = "renderer-fused-materialize-project")]
                        {
                            self.flush_fused_world_batch(
                                map,
                                batch_vertices.as_mut_ptr().cast(),
                                batch_vertex_count,
                                batch_surfaces.as_ptr().cast(),
                                batch_indexed_sources.as_ptr().cast(),
                                batch_visible_indices.as_ptr().cast(),
                                batch_surface_count,
                                next,
                            )
                        }
                        #[cfg(not(feature = "renderer-fused-materialize-project"))]
                        {
                            flush_world_batch(
                                batch_vertices.as_mut_ptr().cast(),
                                batch_vertex_count,
                                batch_surfaces.as_ptr().cast(),
                                batch_surface_count,
                                next,
                            )
                        }
                    };
                    #[cfg(feature = "renderer-subdivision-cache")]
                    let submitted = unsafe {
                        self.flush_cached_subdivision_batch(
                            batch_vertices.as_mut_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_source_surfaces.as_ptr().cast(),
                            batch_surface_count,
                            next,
                            &mut pending_world_start,
                            &mut stats,
                        )
                    };
                    #[cfg(feature = "renderer-indexed-projection")]
                    let submitted = unsafe {
                        self.flush_indexed_world_batch(
                            map,
                            batch_vertices.as_mut_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_surface_count,
                            next,
                            &mut stats,
                        )
                    };
                    #[cfg(feature = "renderer-topology-cache")]
                    let resident_submitted = unsafe {
                        self.flush_resident_world_batch(
                            batch_vertices.as_mut_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_surface_count,
                            start,
                            next,
                            topology_batch_index,
                            topology_batch_identity,
                        )
                    };
                    #[cfg(feature = "renderer-topology-cache")]
                    let submitted = resident_submitted.submit;
                    #[cfg(feature = "renderer-topology-cache")]
                    if batch_surface_count != 0 {
                        record_topology_cache_submit(&mut stats, resident_submitted);
                        topology_batch_index += 1;
                        topology_batch_identity = TopologyBatchIdentity::new(map.generation());
                    }
                    #[cfg(all(
                        feature = "renderer-census",
                        not(feature = "renderer-topology-cache")
                    ))]
                    unsafe {
                        census_world_batch(
                            batch_vertices.as_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_surface_count,
                            batch_source_surfaces.as_ptr().cast(),
                            batch_output,
                            submitted,
                            map.generation(),
                            self.frame,
                            &mut self.subdivision_slab_cache_models,
                            &mut renderer_census,
                        );
                    }
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    batch_vertex_count = 0;
                    batch_surface_count = 0;
                    batch_worst_words = 0;

                    let face_worst_words =
                        (reserve_count - 2) * WORST_WINDOWED_PACKET_WORDS_PER_TRIANGLE;
                    if !packet_capacity(next, end, face_worst_words) {
                        stats.packet_overflow_avoided = true;
                        break;
                    }
                    let vertices = unsafe { batch_vertices_mut(batch_vertices, 0, vertex_count) };
                    #[cfg(not(feature = "renderer-hoisted-indexed-world"))]
                    self.materialize_retained_face(map, face, texture, vertices);
                    #[cfg(feature = "renderer-hoisted-indexed-world")]
                    self.materialize_retained_face_from_indexed(
                        indexed_world,
                        face,
                        texture,
                        vertices,
                    );
                    animate_special_surface(vertices, texture, self.frame);
                    let vertex_count = if clip {
                        unsafe { clip_face_near(batch_vertices.as_mut_ptr().cast(), vertex_count) }
                    } else {
                        vertex_count
                    };
                    if vertex_count < 3 {
                        continue;
                    }
                    #[cfg(feature = "renderer-window-range-coalescing")]
                    let window_packet_start = next;
                    let submitted = unsafe {
                        submit_classic_affine_scoped_windowed_fan(
                            batch_vertices.as_mut_ptr().cast(),
                            vertex_count,
                            next,
                            if water_blend {
                                texture.texture_page | 0x60
                            } else {
                                texture.texture_page
                            },
                            if water_blend {
                                clut_liquid()
                            } else {
                                clut_texture()
                            },
                            special_texture_window(texture).word(),
                            ClassicAffineProfile::QUAKE_REFERENCE,
                        )
                    };
                    if water_blend {
                        // The shared fan owns the costly projection,
                        // subdivision and scoped-window packet topology. Its
                        // only opaque policy is GP0(34h/3Ch); flip bit 25 in
                        // each emitted color command to GP0(36h/3Eh).
                        unsafe {
                            mark_window_packets_translucent(next, submitted.next_packet);
                        }
                    }
                    #[cfg(feature = "renderer-window-range-coalescing")]
                    unsafe {
                        crate::platform::register_world_window_packet_range(
                            window_packet_start,
                            submitted.next_packet,
                        );
                    }
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    stats.visible_faces = stats.visible_faces.saturating_add(1);
                    continue;
                }

                // Near clipping can create one interpolated vertex which has
                // no cooked shared-position index. Keep those rare faces on
                // the authoritative project-all path, separated by exact
                // batch boundaries so the indexed stream remains valid.
                #[cfg(feature = "renderer-indexed-projection")]
                if clip {
                    let submitted = unsafe {
                        self.flush_indexed_world_batch(
                            map,
                            batch_vertices.as_mut_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_surface_count,
                            next,
                            &mut stats,
                        )
                    };
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    batch_vertex_count = 0;
                    batch_surface_count = 0;
                    batch_worst_words = 0;

                    let face_worst_words = (reserve_count - 2) * WORST_PACKET_WORDS_PER_TRIANGLE;
                    if !packet_capacity(next, end, face_worst_words) {
                        stats.packet_overflow_avoided = true;
                        break;
                    }
                    let vertices = unsafe { batch_vertices_mut(batch_vertices, 0, reserve_count) };
                    #[cfg(not(feature = "renderer-hoisted-indexed-world"))]
                    self.materialize_retained_face(
                        map,
                        face,
                        texture,
                        &mut vertices[..vertex_count],
                    );
                    #[cfg(feature = "renderer-hoisted-indexed-world")]
                    self.materialize_retained_face_from_indexed(
                        indexed_world,
                        face,
                        texture,
                        &mut vertices[..vertex_count],
                    );
                    if self.frame_light.is_some() {
                        self.light_face(visible_index, &mut vertices[..vertex_count]);
                    }
                    let clipped_count =
                        unsafe { clip_face_near(vertices.as_mut_ptr(), vertex_count) };
                    if clipped_count < 3 {
                        continue;
                    }
                    let surface = ClassicAffineBatchSurface {
                        first_vertex: 0,
                        vertex_count: clipped_count as u16,
                        tpage: texture.texture_page,
                        clut: clut_texture(),
                    };
                    let submitted = unsafe {
                        flush_batch(vertices.as_mut_ptr(), clipped_count, &surface, 1, next)
                    };
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    stats.visible_faces = stats.visible_faces.saturating_add(1);
                    continue;
                }

                #[cfg(feature = "renderer-fused-materialize-project")]
                if clip {
                    let submitted = unsafe {
                        self.flush_fused_world_batch(
                            map,
                            batch_vertices.as_mut_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_indexed_sources.as_ptr().cast(),
                            batch_visible_indices.as_ptr().cast(),
                            batch_surface_count,
                            next,
                        )
                    };
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    batch_vertex_count = 0;
                    batch_surface_count = 0;
                    batch_worst_words = 0;

                    let face_worst_words = (reserve_count - 2) * WORST_PACKET_WORDS_PER_TRIANGLE;
                    if !packet_capacity(next, end, face_worst_words) {
                        stats.packet_overflow_avoided = true;
                        break;
                    }
                    let vertices = unsafe { batch_vertices_mut(batch_vertices, 0, reserve_count) };
                    self.materialize_retained_face(
                        map,
                        face,
                        texture,
                        &mut vertices[..vertex_count],
                    );
                    if self.frame_light.is_some() {
                        self.light_face(visible_index, &mut vertices[..vertex_count]);
                    }
                    let clipped_count =
                        unsafe { clip_face_near(vertices.as_mut_ptr(), vertex_count) };
                    if clipped_count < 3 {
                        continue;
                    }
                    let surface = ClassicAffineBatchSurface {
                        first_vertex: 0,
                        vertex_count: clipped_count as u16,
                        tpage: texture.texture_page,
                        clut: clut_texture(),
                    };
                    let submitted = unsafe {
                        flush_batch(vertices.as_mut_ptr(), clipped_count, &surface, 1, next)
                    };
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    stats.visible_faces = stats.visible_faces.saturating_add(1);
                    continue;
                }

                let face_worst_words = (reserve_count - 2) * WORST_PACKET_WORDS_PER_TRIANGLE;
                if batch_vertex_count + reserve_count > BATCH_MAX_VERTICES
                    || batch_surface_count == BATCH_MAX_SURFACES
                    || !packet_capacity(next, end, batch_worst_words + face_worst_words)
                {
                    #[cfg(all(
                        feature = "renderer-census",
                        not(feature = "renderer-topology-cache")
                    ))]
                    let batch_output = next;
                    #[cfg(all(
                        not(feature = "renderer-topology-cache"),
                        not(feature = "renderer-indexed-projection"),
                        not(feature = "renderer-subdivision-cache")
                    ))]
                    let submitted = unsafe {
                        #[cfg(feature = "renderer-fused-materialize-project")]
                        {
                            self.flush_fused_world_batch(
                                map,
                                batch_vertices.as_mut_ptr().cast(),
                                batch_vertex_count,
                                batch_surfaces.as_ptr().cast(),
                                batch_indexed_sources.as_ptr().cast(),
                                batch_visible_indices.as_ptr().cast(),
                                batch_surface_count,
                                next,
                            )
                        }
                        #[cfg(not(feature = "renderer-fused-materialize-project"))]
                        {
                            flush_world_batch(
                                batch_vertices.as_mut_ptr().cast(),
                                batch_vertex_count,
                                batch_surfaces.as_ptr().cast(),
                                batch_surface_count,
                                next,
                            )
                        }
                    };
                    #[cfg(feature = "renderer-subdivision-cache")]
                    let submitted = unsafe {
                        self.flush_cached_subdivision_batch(
                            batch_vertices.as_mut_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_source_surfaces.as_ptr().cast(),
                            batch_surface_count,
                            next,
                            &mut pending_world_start,
                            &mut stats,
                        )
                    };
                    #[cfg(feature = "renderer-indexed-projection")]
                    let submitted = unsafe {
                        self.flush_indexed_world_batch(
                            map,
                            batch_vertices.as_mut_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_surface_count,
                            next,
                            &mut stats,
                        )
                    };
                    #[cfg(feature = "renderer-topology-cache")]
                    let resident_submitted = unsafe {
                        self.flush_resident_world_batch(
                            batch_vertices.as_mut_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_surface_count,
                            start,
                            next,
                            topology_batch_index,
                            topology_batch_identity,
                        )
                    };
                    #[cfg(feature = "renderer-topology-cache")]
                    let submitted = resident_submitted.submit;
                    #[cfg(feature = "renderer-topology-cache")]
                    if batch_surface_count != 0 {
                        record_topology_cache_submit(&mut stats, resident_submitted);
                        topology_batch_index += 1;
                        topology_batch_identity = TopologyBatchIdentity::new(map.generation());
                    }
                    #[cfg(all(
                        feature = "renderer-census",
                        not(feature = "renderer-topology-cache")
                    ))]
                    unsafe {
                        census_world_batch(
                            batch_vertices.as_ptr().cast(),
                            batch_vertex_count,
                            batch_surfaces.as_ptr().cast(),
                            batch_surface_count,
                            batch_source_surfaces.as_ptr().cast(),
                            batch_output,
                            submitted,
                            map.generation(),
                            self.frame,
                            &mut self.subdivision_slab_cache_models,
                            &mut renderer_census,
                        );
                    }
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    batch_vertex_count = 0;
                    batch_surface_count = 0;
                    batch_worst_words = 0;
                }
                if !packet_capacity(next, end, face_worst_words) {
                    stats.packet_overflow_avoided = true;
                    break;
                }

                #[cfg(not(feature = "renderer-fused-materialize-project"))]
                let vertices = unsafe {
                    batch_vertices_mut(batch_vertices, batch_vertex_count, reserve_count)
                };
                #[cfg(not(feature = "renderer-indexed-projection"))]
                {
                    #[cfg(all(
                        not(feature = "renderer-fused-materialize-project"),
                        not(feature = "renderer-hoisted-indexed-world")
                    ))]
                    self.materialize_retained_face(
                        map,
                        face,
                        texture,
                        &mut vertices[..vertex_count],
                    );
                    #[cfg(all(
                        not(feature = "renderer-fused-materialize-project"),
                        feature = "renderer-hoisted-indexed-world"
                    ))]
                    self.materialize_retained_face_from_indexed(
                        indexed_world,
                        face,
                        texture,
                        &mut vertices[..vertex_count],
                    );
                }
                #[cfg(feature = "renderer-indexed-projection")]
                self.materialize_indexed_world_face(
                    map,
                    face,
                    texture,
                    batch_vertex_count,
                    &mut vertices[..vertex_count],
                );
                // Before the near clip, which interpolates the corner colours
                // of whichever vertices it keeps.
                #[cfg(not(feature = "renderer-fused-materialize-project"))]
                if self.frame_light.is_some() {
                    self.light_face(visible_index, &mut vertices[..vertex_count]);
                }
                #[cfg(not(feature = "renderer-fused-materialize-project"))]
                let vertex_count = if clip {
                    unsafe { clip_face_near(vertices.as_mut_ptr(), vertex_count) }
                } else {
                    vertex_count
                };
                #[cfg(feature = "renderer-fused-materialize-project")]
                let vertex_count = vertex_count;
                if vertex_count < 3 {
                    continue;
                }
                #[cfg(not(feature = "renderer-topology-cache"))]
                batch_surfaces[batch_surface_count].write(ClassicAffineBatchSurface {
                    first_vertex: batch_vertex_count as u16,
                    vertex_count: vertex_count as u16,
                    tpage: texture.texture_page,
                    clut: clut_texture(),
                });
                #[cfg(feature = "renderer-fused-materialize-project")]
                {
                    let face_flags = u16::from(face.flags);
                    let format = u16::from(face_flags & FACE_BAKED_UV != 0)
                        | (u16::from(face_flags & FACE_BAKED_LIGHT != 0) << 1);
                    batch_indexed_sources[batch_surface_count].write(
                        ClassicAffineIndexedBatchSource {
                            first_corner: u32::from(face.first_corner),
                            uv_offset: [texture.atlas.x, texture.atlas.y],
                            format,
                            light_weights: [
                                self.light_styles[face.light_styles[0] as usize],
                                self.light_styles[face.light_styles[1] as usize],
                            ],
                        },
                    );
                    batch_visible_indices[batch_surface_count].write(visible_index as u16);
                }
                #[cfg(any(feature = "renderer-census", feature = "renderer-subdivision-cache"))]
                {
                    #[cfg(feature = "renderer-subdivision-cache")]
                    let stable = u16::from(face.flags) & (FACE_BAKED_UV | FACE_BAKED_LIGHT)
                        == FACE_BAKED_UV | FACE_BAKED_LIGHT
                        && self.frame_light.is_none_or(|light| {
                            dynamic_light_misses(light, visible.bounds.mins, visible.bounds.maxs)
                        });
                    #[cfg(not(feature = "renderer-subdivision-cache"))]
                    let stable = true;
                    batch_source_surfaces[batch_surface_count].write(if clip || !stable {
                        u16::MAX
                    } else {
                        visible.bounds.surface_index
                    });
                }
                #[cfg(feature = "renderer-topology-cache")]
                {
                    let face_flags = u16::from(face.flags);
                    let stable = face_flags & (FACE_BAKED_UV | FACE_BAKED_LIGHT)
                        == FACE_BAKED_UV | FACE_BAKED_LIGHT
                        && self.frame_light.is_none_or(|light| {
                            dynamic_light_misses(light, visible_bounds.mins, visible_bounds.maxs)
                        });
                    let clut = clut_texture();
                    batch_surfaces[batch_surface_count].write(ClassicAffineResidentBatchSurface {
                        first_vertex: batch_vertex_count as u16,
                        vertex_count: vertex_count as u16,
                        tpage: texture.texture_page,
                        clut,
                        reuse_invariants: u8::from(stable),
                        _padding: [0; 3],
                    });
                    topology_batch_identity.mix(
                        visible_bounds.surface_index,
                        texture.texture_page,
                        clut,
                        stable,
                    );
                }
                batch_vertex_count += vertex_count;
                batch_surface_count += 1;
                batch_worst_words += (vertex_count - 2) * WORST_PACKET_WORDS_PER_TRIANGLE;
                stats.visible_faces = stats.visible_faces.saturating_add(1);
            }

            #[cfg(all(feature = "renderer-census", not(feature = "renderer-topology-cache")))]
            let batch_output = next;
            #[cfg(all(
                not(feature = "renderer-topology-cache"),
                not(feature = "renderer-indexed-projection"),
                not(feature = "renderer-subdivision-cache")
            ))]
            let submitted = unsafe {
                #[cfg(feature = "renderer-fused-materialize-project")]
                {
                    self.flush_fused_world_batch(
                        map,
                        batch_vertices.as_mut_ptr().cast(),
                        batch_vertex_count,
                        batch_surfaces.as_ptr().cast(),
                        batch_indexed_sources.as_ptr().cast(),
                        batch_visible_indices.as_ptr().cast(),
                        batch_surface_count,
                        next,
                    )
                }
                #[cfg(not(feature = "renderer-fused-materialize-project"))]
                {
                    flush_world_batch(
                        batch_vertices.as_mut_ptr().cast(),
                        batch_vertex_count,
                        batch_surfaces.as_ptr().cast(),
                        batch_surface_count,
                        next,
                    )
                }
            };
            #[cfg(feature = "renderer-subdivision-cache")]
            let submitted = unsafe {
                self.flush_cached_subdivision_batch(
                    batch_vertices.as_mut_ptr().cast(),
                    batch_vertex_count,
                    batch_surfaces.as_ptr().cast(),
                    batch_source_surfaces.as_ptr().cast(),
                    batch_surface_count,
                    next,
                    &mut pending_world_start,
                    &mut stats,
                )
            };
            #[cfg(feature = "renderer-indexed-projection")]
            let submitted = unsafe {
                self.flush_indexed_world_batch(
                    map,
                    batch_vertices.as_mut_ptr().cast(),
                    batch_vertex_count,
                    batch_surfaces.as_ptr().cast(),
                    batch_surface_count,
                    next,
                    &mut stats,
                )
            };
            #[cfg(feature = "renderer-topology-cache")]
            let resident_submitted = unsafe {
                self.flush_resident_world_batch(
                    batch_vertices.as_mut_ptr().cast(),
                    batch_vertex_count,
                    batch_surfaces.as_ptr().cast(),
                    batch_surface_count,
                    start,
                    next,
                    topology_batch_index,
                    topology_batch_identity,
                )
            };
            #[cfg(feature = "renderer-topology-cache")]
            let submitted = resident_submitted.submit;
            #[cfg(feature = "renderer-topology-cache")]
            if batch_surface_count != 0 {
                record_topology_cache_submit(&mut stats, resident_submitted);
            }
            #[cfg(all(feature = "renderer-census", not(feature = "renderer-topology-cache")))]
            unsafe {
                census_world_batch(
                    batch_vertices.as_ptr().cast(),
                    batch_vertex_count,
                    batch_surfaces.as_ptr().cast(),
                    batch_surface_count,
                    batch_source_surfaces.as_ptr().cast(),
                    batch_output,
                    submitted,
                    map.generation(),
                    self.frame,
                    &mut self.subdivision_slab_cache_models,
                    &mut renderer_census,
                );
            }
            next = submitted.next_packet;
            stats.packets = stats.packets.wrapping_add(submitted.packets);
            stats.hardware_triangles = stats
                .hardware_triangles
                .wrapping_add(submitted.hardware_triangles);
        }

        #[cfg(feature = "renderer-static-world-reuse")]
        if visibility_valid && !static_world_cache_hit {
            let cache = unsafe { self.static_world_cache.get_unchecked_mut(self.arena) };
            let world_words = unsafe { next.offset_from(start) as usize };
            if static_world_cache_eligible
                && !stats.packet_overflow_avoided
                && world_words <= u16::MAX as usize
                && unsafe { capture_static_world_tags(start, next, &mut cache.tag_slots) }
            {
                cache.key = Some(static_world_key);
                cache.world_words = world_words as u16;
                cache.packets = stats.packets;
                cache.hardware_triangles = stats.hardware_triangles;
                cache.visible_faces = stats.visible_faces;
                cache.layered_sky_texture = layered_sky_texture;
            } else {
                cache.invalidate();
            }
        }

        if visibility_valid && !stats.packet_overflow_avoided {
            next = self.draw_entities(
                map,
                entities,
                rotating_yaw,
                camera,
                view,
                next,
                end,
                &mut stats,
            );
        }

        next = self.draw_explosion_effects(explosion_effects, view, next, end, &mut stats);
        next =
            self.draw_impact_particles(map, impact_particles, camera, view, next, end, &mut stats);

        if let Some(beam) = lightning_beam {
            next = self.draw_lightning_beam(map, beam, view, next, end, &mut stats);
        }

        // Submit the sky last into the farthest OT slot. OT insertion is
        // prepend-only, so this makes the bounded screen lattice execute
        // before every world/entity polygon and lets opaque geometry mask it.
        if let Some(texture) = layered_sky_texture {
            if packet_capacity(next, end, SKY_BACKGROUND_WORDS) {
                let submitted =
                    unsafe { submit_view_ray_sky_background(texture, view, self.frame, next) };
                next = submitted.next_packet;
                stats.packets = stats.packets.wrapping_add(submitted.packets);
                stats.hardware_triangles = stats
                    .hardware_triangles
                    .wrapping_add(submitted.hardware_triangles);
            } else {
                stats.packet_overflow_avoided = true;
            }
        }

        if let Some(view_model) = view_model {
            self.draw_view_model(map, camera, view_model, next, end, &mut stats);
        }

        if let Some(hud) = hud {
            let hud_stats = self.draw_hud(map, hud);
            stats.hud_packets = hud_stats.generated;
            #[cfg(feature = "visual-parity-regression")]
            {
                stats.hud_registered_packets = hud_stats.hud_registered;
                stats.crosshair_registered_packets = hud_stats.crosshair_registered;
            }
            stats.packets = stats.packets.wrapping_add(stats.hud_packets);
            stats.hardware_triangles = stats.hardware_triangles.wrapping_add(
                stats
                    .hud_packets
                    .saturating_sub(hud_stats.crosshair_registered)
                    .saturating_mul(2),
            );
        }

        // The screen blends sit under the HUD text and over everything the
        // world drew, which is where the original's palette shift sits too.
        self.draw_screen_tints(screen_blend);

        if let Some(text) = centerprint {
            self.draw_centerprint(text);
        }

        if let Some(menu) = menu {
            self.draw_menu(map, menu);
        }

        if let Some(intermission) = intermission {
            self.draw_intermission(intermission);
        }

        // Not over the intermission panel or Options. That menu already names
        // the selected track, and its eleven shadowed rows plus the longest
        // banner exceed the 232-quad menu allocation. Keeping the transient
        // banner off that page leaves the reachable Levels peak unchanged.
        if let Some((track, elapsed)) = now_playing {
            if intermission.is_none()
                && !matches!(menu, Some(view) if view.page == MenuPage::Options)
            {
                self.draw_now_playing(track, elapsed, menu.is_some());
            }
        }

        // Last, so the transition takes the panel and the HUD down with the
        // world rather than leaving text floating over black.
        self.draw_screen_fade(screen_blend);

        #[cfg(feature = "visual-parity-regression")]
        {
            stats.screen_registered_packets =
                crate::platform::registered_screen_packet_count() as u32;
            let audit = unsafe { audit_scoped_window_packets(start, next) };
            stats.scoped_window_packets = audit.windowed;
            stats.scoped_window_resets = audit.restored;
            stats.scoped_window_reset_failures = audit.failures;
        }

        #[cfg(feature = "episode1-regression")]
        {
            // `start` and `next` are members of the same 128 KiB arena.
            stats.packet_arena_words = unsafe { next.offset_from(start) as u32 };
        }

        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::stage_end(psx_telemetry::stage::RENDER);
        #[cfg(feature = "renderer-census")]
        {
            renderer_census.packet_arena_words = unsafe { next.offset_from(start) as u32 };
            renderer_census.emitted_packets = stats.packets;
            renderer_census.hardware_triangles = stats.hardware_triangles;
            renderer_census.packet_overflow_avoided = u32::from(stats.packet_overflow_avoided);
            emit_renderer_census(self.frame, &renderer_census);
        }
        #[cfg(not(feature = "renderer-subdivision-cache"))]
        unsafe {
            crate::platform::gpu_end_frame(start, next)
        };
        #[cfg(feature = "renderer-subdivision-cache")]
        unsafe {
            crate::platform::gpu_insert_world_stream(pending_world_start, next);
            crate::platform::gpu_end_frame(ptr::null_mut(), ptr::null_mut());
        };
        self.arena ^= 1;
        self.frame = self.frame.wrapping_add(1);
        stats
    }

    /// Stage the original owned/inactive and selected weapon pixels over the
    /// seven packed strip slots. A switch touches only the old and new slot;
    /// the upload shares the frame's existing GPU fence with liquid animation.
    #[optimize(size)]
    #[inline(never)]
    fn stage_weapon_icons(&mut self, map: &ResidentMap, view: HudView) {
        let desired = view
            .active_weapon_slot()
            .map(|index| 1u8 << index)
            .unwrap_or(0);
        let changed = self.weapon_selected_mask ^ desired;
        if changed == 0 {
            return;
        }
        const EMPTY: crate::platform::VramUploadRange = crate::platform::VramUploadRange {
            rect: psx_vram::VramRect::new(0, 0, 1, 1),
            start: 0,
            len: 2,
        };
        let mut uploads = [EMPTY; 2];
        let mut count = 0usize;
        for index in 0..7 {
            if changed & (1 << index) == 0 {
                continue;
            }
            let Some(picture) =
                map.picture_at(GraphicsPictureId::InventoryWeaponShotgun.index() + index)
            else {
                return;
            };
            let Some(rect) = picture_upload_rect(picture) else {
                return;
            };
            let variant = if desired & (1 << index) != 0 {
                GRAPHICS_WEAPON_ICON_VARIANT_BYTES
            } else {
                0
            };
            uploads[count] = crate::platform::VramUploadRange {
                rect,
                start: variant + GRAPHICS_WEAPON_ICON_OFFSETS[index],
                len: GRAPHICS_WEAPON_ICON_OFFSETS[index + 1] - GRAPHICS_WEAPON_ICON_OFFSETS[index],
            };
            count += 1;
        }
        let staged = unsafe {
            crate::platform::defer_vram_ranges(map.weapon_icon_pixels(), &uploads[..count])
        };
        if staged.is_ok() {
            self.weapon_selected_mask = desired;
        }
    }

    #[optimize(size)]
    fn draw_hud(&mut self, map: &ResidentMap, view: HudView) -> HudPacketStats {
        let packets = &mut self.hud_packets[self.arena];
        packets.clear();

        let graphical = match unsafe { HUD_MODE } {
            HudMode::Minimal => push_minimal_hud(packets, map, view),
            HudMode::Classic => push_classic_hud(packets, map, view),
        };
        if !graphical {
            packets.clear();
            push_text_hud(packets, view);
        }

        #[cfg(feature = "start-route-regression")]
        unsafe {
            static mut REPORTED: bool = false;
            if !REPORTED {
                REPORTED = true;
                if graphical {
                    psx_telemetry::emit::debug_log("quake-psx: HUD graphical packets ready");
                } else {
                    psx_telemetry::emit::debug_log("quake-psx: HUD fell back to text");
                }
            }
        }

        debug_assert!(packets.len() <= HUD_PACKET_CAPACITY);
        let hud_registered = register_hud_packets(packets);
        let crosshair_len = if unsafe { CROSSHAIR } {
            self.crosshair_packets[self.arena].len()
        } else {
            0
        };
        let crosshair_registered = unsafe {
            crate::platform::register_screen_packets(
                self.crosshair_packets[self.arena]
                    .as_mut_ptr()
                    .cast::<u32>(),
                crosshair_len,
                RectFlat::WORDS,
            )
        };
        HudPacketStats {
            generated: (packets.len() + crosshair_len) as u32,
            hud_registered: hud_registered as u32,
            crosshair_registered: crosshair_registered as u32,
        }
    }

    /// Draw and expose one complete loading frame before a blocking CD read.
    #[optimize(size)]
    pub fn draw_loading(&mut self, disc: GraphicsPicture, map: EpisodeMap) {
        crate::platform::gpu_begin_frame();
        let packets = &mut self.menu_packets[self.arena];
        packets.clear();
        push_picture(
            packets,
            320 - i16::from(disc.width) - 16,
            16,
            disc,
            (0x80, 0x80, 0x80),
        );
        push_centered_text(packets, 102, "LOADING", (0x80, 0x64, 0x38));
        push_centered_text(packets, 118, map.display_name(), (0x80, 0x80, 0x80));
        unsafe {
            crate::platform::register_screen_packets(
                packets.as_mut_ptr().cast::<u32>(),
                packets.len(),
                QuadTexturedMaterial::WORDS,
            );
            crate::platform::gpu_end_frame(core::ptr::null_mut(), core::ptr::null_mut());
        }
        crate::platform::gpu_present_pending_frame();
        #[cfg(feature = "start-route-regression")]
        psx_telemetry::emit::debug_log("quake-psx: loading frame presented");
        self.arena ^= 1;
    }

    /// Quake's `V_UpdatePalette`, as at most two full-screen quads.
    ///
    /// Out of line: two quads a frame is nothing beside the per-face world
    /// loop above it, and inlined here the tint construction and the packet
    /// registration land in the middle of `draw_frame`'s register pressure.
    #[optimize(size)]
    #[inline(never)]
    fn draw_screen_tints(&mut self, blend: &quake_core::screenblend::ScreenBlend) {
        let slots = &mut self.screen_tints[self.arena];
        let mut count = 0usize;
        for tint in [blend.contents_tint(), blend.flash_tint()]
            .into_iter()
            .flatten()
        {
            let mode = if tint.average {
                BlendMode::Average
            } else {
                BlendMode::Add
            };
            slots[count] = ScreenTintQuad::new(tint.color, mode);
            count += 1;
        }
        if count == 0 {
            return;
        }
        unsafe {
            crate::platform::register_screen_packets(
                slots.as_mut_ptr().cast::<u32>(),
                count,
                ScreenTintQuad::WORDS,
            );
        }
    }

    /// The level-transition fade to and from black.
    ///
    /// PORT ADDITION, not id1: the original cuts straight into the end-of-level
    /// panel and straight out of it into the next map. This rides the same
    /// full-screen quad the palette blends use, one subtract instead of an add,
    /// and is registered last so it darkens the panel and the HUD as well as
    /// the world. `quake.rs` is the only thing that starts it.
    #[optimize(size)]
    fn draw_screen_fade(&mut self, blend: &quake_core::screenblend::ScreenBlend) {
        let Some(shade) = blend.transition_shade() else {
            return;
        };
        let slots = &mut self.screen_tints[self.arena];
        slots[SCREEN_FADE_SLOT] = ScreenTintQuad::new((shade, shade, shade), BlendMode::Subtract);
        unsafe {
            crate::platform::register_screen_packets(
                slots[SCREEN_FADE_SLOT..].as_mut_ptr().cast::<u32>(),
                1,
                ScreenTintQuad::WORDS,
            );
        }
    }

    /// One authored `centerprint` block, drawn over the HUD in the shared font.
    ///
    /// Out of line, and this is the larger of the two: the text layout it
    /// calls only runs on the frames that carry a centerprint, but inlined it
    /// unrolls a per-glyph loop into `draw_frame` and spills the world loop's
    /// invariants around it.
    #[inline(never)]
    #[optimize(size)]
    fn draw_centerprint(&mut self, text: &str) {
        let packets = &mut self.centerprint_packets[self.arena];
        packets.clear();
        let x = quake_core::text::centered_first_line_x(text, SCREEN_WIDTH, 8, 40);
        push_centerprint_text(packets, x, 96, text, (0x80, 0x80, 0x80));
        debug_assert!(packets.len() <= CENTERPRINT_PACKET_CAPACITY);
        unsafe {
            crate::platform::register_screen_packets(
                packets.as_mut_ptr().cast::<u32>(),
                packets.len(),
                QuadTexturedMaterial::WORDS,
            );
        }
    }

    fn draw_lightning_beam(
        &mut self,
        map: &ResidentMap,
        beam: LightningBeam,
        view: QuakeViewTransform,
        mut output: *mut u32,
        end: *mut u32,
        stats: &mut RenderStats,
    ) -> *mut u32 {
        const SEGMENT_UNITS: i32 = 30;
        let Some(model) = map.alias_models().get(LIGHTNING_BOLT_MODEL_ID) else {
            stats.packet_overflow_avoided = true;
            return output;
        };
        let header = model.header();
        let face_count = header.triangle_count as usize;
        let Some(worst_words) = face_count.checked_mul(ALIAS_PACKET_WORDS) else {
            stats.packet_overflow_avoided = true;
            return output;
        };
        let vertices = model.frame_bytes(0).expect("validated lightning frame");
        let faces = model.triangle_bytes(0).expect("validated lightning skin");

        let dx = (beam.end.x.saturating_sub(beam.start.x)) >> 12;
        let dy = (beam.end.y.saturating_sub(beam.start.y)) >> 12;
        let dz = (beam.end.z.saturating_sub(beam.start.z)) >> 12;
        let horizontal =
            isqrt_i32(square_i32_saturating(dx).saturating_add(square_i32_saturating(dy)));
        let length =
            isqrt_i32(square_i32_saturating(horizontal).saturating_add(square_i32_saturating(dz)));
        if length == 0 {
            return output;
        }
        let pitch = atan2_q12(-dz, horizontal) as u16;
        let yaw = atan2_q12(dy, dx) as u16;
        let direction = Vec3I32 {
            x: dx.saturating_mul(1 << 12) / length,
            y: dy.saturating_mul(1 << 12) / length,
            z: dz.saturating_mul(1 << 12) / length,
        };
        let step = Vec3I32 {
            x: direction.x.saturating_mul(SEGMENT_UNITS),
            y: direction.y.saturating_mul(SEGMENT_UNITS),
            z: direction.z.saturating_mul(SEGMENT_UNITS),
        };
        let mut origin = beam.start;
        let mut remaining = length;
        let mut segment = 0u32;
        while remaining > 0 {
            if !packet_capacity(output, end, worst_words) {
                stats.packet_overflow_avoided = true;
                break;
            }
            // Vanilla chooses a fresh random 0..359 roll for every 30-unit
            // `bolt2.mdl` copy on every client frame. A deterministic mix of
            // frame and segment preserves that restless silhouette without
            // making captures or replays nondeterministic.
            let roll = self
                .frame
                .wrapping_mul(73)
                .wrapping_add(segment.wrapping_mul(151))
                .wrapping_rem(256) as u16;
            let model_rotation = Mat3I16::rotate_z(yaw >> 4)
                .mul(&Mat3I16::rotate_y(pitch >> 4))
                .mul(&Mat3I16::rotate_x(roll));
            let (rotation, translation) = compose_classic_alias_transform(
                view.rotation,
                view.translation,
                model_rotation,
                GteVec3I16::new(header.offset.x, header.offset.y, header.offset.z),
                GteVec3I32::new(origin.x >> 12, origin.y >> 12, origin.z >> 12),
                GteVec3I16::new(header.scale.x, header.scale.y, header.scale.z),
            );
            scene::load_rotation(&rotation);
            scene::load_translation(translation);
            let submitted = unsafe {
                submit_classic_alias_model(
                    vertices.as_ptr().cast::<ClassicAliasVertex>(),
                    header.vertex_count as usize,
                    faces.as_ptr().cast::<ClassicAliasFace>(),
                    face_count,
                    self.alias_projected.as_mut_ptr(),
                    output,
                    header.skins[0].texture_page,
                    clut_texture(),
                    0x0080_8080,
                    ClassicAffineProfile::QUAKE_REFERENCE,
                )
            };
            output = submitted.next_packet;
            stats.lightning_beam_packets = stats
                .lightning_beam_packets
                .saturating_add(submitted.packets);
            stats.packets = stats.packets.saturating_add(submitted.packets);
            stats.hardware_triangles = stats
                .hardware_triangles
                .saturating_add(submitted.hardware_triangles);
            origin.x = origin.x.saturating_add(step.x);
            origin.y = origin.y.saturating_add(step.y);
            origin.z = origin.z.saturating_add(step.z);
            remaining -= SEGMENT_UNITS;
            segment = segment.wrapping_add(1);
        }
        output
    }

    fn draw_explosion_effects(
        &self,
        effects: impl Iterator<Item = ExplosionEffect>,
        view: QuakeViewTransform,
        mut output: *mut u32,
        end: *mut u32,
        stats: &mut RenderStats,
    ) -> *mut u32 {
        const RAYS: [(i32, i32, i32); 6] = [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ];
        const PACKET_WORDS: usize = LineMono::WORDS as usize + 1;
        scene::load_rotation(&view.rotation);
        scene::load_translation(view.translation);
        let vertex = |point: Vec3I32| {
            GteVec3I16::new(
                (point.x >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                (point.y >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                (point.z >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            )
        };
        for effect in effects {
            let center = scene::project_vertex(vertex(effect.origin));
            if center.sz == 0 {
                continue;
            }
            let radius = effect.radius_units().saturating_mul(4096);
            let color = effect.color();
            for (x, y, z) in RAYS {
                if !packet_capacity(output, end, PACKET_WORDS) {
                    stats.packet_overflow_avoided = true;
                    return output;
                }
                let endpoint = Vec3I32 {
                    x: effect.origin.x.saturating_add(radius.saturating_mul(x)),
                    y: effect.origin.y.saturating_add(radius.saturating_mul(y)),
                    z: effect.origin.z.saturating_add(radius.saturating_mul(z)),
                };
                let projected = scene::project_vertex(vertex(endpoint));
                if projected.sz == 0 {
                    continue;
                }
                let mut line = LineMono::new(
                    center.sx,
                    center.sy,
                    projected.sx,
                    projected.sy,
                    color.0,
                    color.1,
                    color.2,
                );
                line.tag = u32::from(LineMono::WORDS) << 24;
                unsafe { output.cast::<LineMono>().write(line) };
                output = unsafe { output.add(PACKET_WORDS) };
                stats.explosion_effect_packets = stats.explosion_effect_packets.saturating_add(1);
                stats.packets = stats.packets.saturating_add(1);
            }
        }
        output
    }

    fn draw_impact_particles(
        &self,
        map: &ResidentMap,
        particles: impl Iterator<Item = ImpactParticle>,
        camera: Camera,
        view: QuakeViewTransform,
        mut output: *mut u32,
        end: *mut u32,
        stats: &mut RenderStats,
    ) -> *mut u32 {
        const FLAT_PACKET_WORDS: usize = RectFlat::WORDS as usize + 1;
        scene::load_rotation(&view.rotation);
        scene::load_translation(view.translation);
        let bubble_model = map.alias_models().get(BUBBLE_SPRITE_MODEL_ID);
        for particle in particles {
            if particle.is_bubble() {
                if let Some(model) = bubble_model {
                    let submitted = draw_sprite_model(
                        model,
                        particle.bubble_frame(),
                        particle.origin,
                        [0; 3],
                        camera,
                        view,
                        output,
                        end,
                    );
                    if submitted.overflow {
                        stats.packet_overflow_avoided = true;
                        return output;
                    }
                    output = submitted.next;
                    if submitted.drawn {
                        stats.sprite_packets = stats.sprite_packets.saturating_add(1);
                        stats.impact_particle_packets =
                            stats.impact_particle_packets.saturating_add(1);
                        stats.packets = stats.packets.saturating_add(1);
                        stats.hardware_triangles = stats.hardware_triangles.saturating_add(2);
                    }
                    continue;
                }
            }
            if !packet_capacity(output, end, FLAT_PACKET_WORDS) {
                stats.packet_overflow_avoided = true;
                return output;
            }
            let point = GteVec3I16::new(
                (particle.origin.x >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                (particle.origin.y >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                (particle.origin.z >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            );
            let projected = scene::project_vertex(point);
            if projected.sz == 0 {
                continue;
            }
            let color = particle.color();
            let size = particle.size();
            let mut rect = RectFlat::new(
                projected.sx.saturating_sub(size / 2),
                projected.sy.saturating_sub(size / 2),
                size as u16,
                size as u16,
                color.0,
                color.1,
                color.2,
            );
            rect.tag = u32::from(RectFlat::WORDS) << 24;
            unsafe { output.cast::<RectFlat>().write(rect) };
            output = unsafe { output.add(FLAT_PACKET_WORDS) };
            stats.impact_particle_packets = stats.impact_particle_packets.saturating_add(1);
            stats.packets = stats.packets.saturating_add(1);
        }
        output
    }

    /// The end-of-level panel, drawn in the menu's own text layer over the
    /// authored `info_intermission` camera.
    ///
    /// Out of line, for the reason `draw_centerprint` and `draw_screen_tints`
    /// already are: this runs on the handful of frames between two levels and
    /// never during one, and inlined it lands between `draw_frame`'s world
    /// loop and the code after it.
    #[optimize(size)]
    #[inline(never)]
    fn draw_intermission(&mut self, view: IntermissionView) {
        let packets = &mut self.menu_packets[self.arena];
        packets.clear();
        if view.episode > IntermissionView::EPISODE_PANEL {
            // `SCR_DrawCenterString` lays the finale out on its authored
            // newlines at an eight-pixel pitch, which is exactly what
            // `TextGlyphs` does, so one push draws a whole page. Plain, like
            // the original's finale text: no panel drop shadow.
            push_text(
                packets,
                IntermissionView::FINALE_X,
                96,
                IntermissionView::finale_text(view.episode),
                (0x80, 0x80, 0x80),
            );
            debug_assert!(packets.len() <= MENU_PACKET_CAPACITY);
            unsafe {
                crate::platform::register_screen_packets(
                    packets.as_mut_ptr().cast::<u32>(),
                    packets.len(),
                    QuadTexturedMaterial::WORDS,
                );
            }
            return;
        }
        let headline = if view.episode != IntermissionView::EPISODE_NONE {
            IntermissionView::EPISODE_HEADLINE
        } else {
            IntermissionView::HEADLINE
        };
        push_centered_text(packets, 48, headline, (0x80, 0x64, 0x38));
        push_centered_text(packets, 68, view.title, (0x80, 0x80, 0x80));
        let (minutes, seconds) = view.time();
        push_shadowed_text(packets, 96, 88, "TIME", (0x80, 0x80, 0x80));
        push_shadowed_clock(packets, 176, 88, minutes, seconds, (0x80, 0x80, 0x80));
        push_shadowed_text(packets, 96, 108, "KILLS", (0x80, 0x80, 0x80));
        push_shadowed_u16(packets, 176, 108, view.kills, (0x80, 0x80, 0x80));
        push_shadowed_text(packets, 208, 108, "/", (0x58, 0x58, 0x58));
        push_shadowed_u16(packets, 224, 108, view.total_kills, (0x58, 0x58, 0x58));
        push_shadowed_text(packets, 96, 128, "SECRETS", (0x80, 0x80, 0x80));
        push_shadowed_u16(packets, 176, 128, view.secrets, (0x80, 0x80, 0x80));
        push_shadowed_text(packets, 208, 128, "/", (0x58, 0x58, 0x58));
        push_shadowed_u16(packets, 224, 128, view.total_secrets, (0x58, 0x58, 0x58));
        if view.episode != IntermissionView::EPISODE_NONE {
            push_centered_text(
                packets,
                160,
                IntermissionView::EPISODE_LINE,
                (0x80, 0x64, 0x38),
            );
        }
        debug_assert!(packets.len() <= MENU_PACKET_CAPACITY);
        unsafe {
            crate::platform::register_screen_packets(
                packets.as_mut_ptr().cast::<u32>(),
                packets.len(),
                QuadTexturedMaterial::WORDS,
            );
        }
    }

    /// The now-playing banner: two right-aligned lines in the upper right that
    /// slide in when a song starts and back out as the banner expires.
    ///
    /// It borrows the menu arena, which is idle for every frame of play, so the
    /// banner costs no allocation on an image with under two kilobytes of bump
    /// allocator left. When the menu drew first its packets are kept and this
    /// only appends, registering the tail so the menu's own registration still
    /// points at what it built; alone, the banner takes the arena over.
    /// `push_text` refuses to grow the vector past its capacity, so neither
    /// registration can be left dangling by a reallocation.
    ///
    /// Outlined: `draw_frame` inlines into the game loop, and a banner that is
    /// up for four seconds out of a level does not belong in that body.
    #[optimize(size)]
    #[inline(never)]
    fn draw_now_playing(&mut self, track: &str, elapsed: u32, appended: bool) {
        /// Right margin the two lines are flushed against.
        const RIGHT: i16 = 312;
        const TOP: i16 = 16;
        /// Ticks the banner takes to slide in, and again to leave.
        const SLIDE: u32 = 12;
        const TITLE: &str = "NOW PLAYING";

        let packets = &mut self.menu_packets[self.arena];
        if !appended {
            packets.clear();
        }
        let start = packets.len();

        // How far the lines still are from their seat: all the way off the
        // right edge on the banner's first tick, home by SLIDE, and out again
        // over the last SLIDE ticks.
        let width = (TITLE.len().max(track.len()) as i16) * 8;
        let travel = (320 - (RIGHT - width)).max(0) as u32;
        let remaining = crate::music::BANNER_TICKS.saturating_sub(elapsed);
        let slide = if elapsed < SLIDE {
            travel * (SLIDE - elapsed) / SLIDE
        } else if remaining < SLIDE {
            travel * (SLIDE - remaining) / SLIDE
        } else {
            0
        };
        let offset = slide as i16;

        push_shadowed_text(
            packets,
            RIGHT - TITLE.len() as i16 * 8 + offset,
            TOP,
            TITLE,
            (0x80, 0x64, 0x38),
        );
        push_shadowed_text(
            packets,
            RIGHT - track.len() as i16 * 8 + offset,
            TOP + 12,
            track,
            (0x80, 0x80, 0x80),
        );

        let appended_packets = packets.len() - start;
        if appended_packets != 0 {
            unsafe {
                crate::platform::register_screen_packets(
                    packets.as_mut_ptr().add(start).cast::<u32>(),
                    appended_packets,
                    QuadTexturedMaterial::WORDS,
                );
            }
        }
    }

    /// The pause and options pages, drawn over whatever is behind them.
    ///
    /// Out of line, and it is the larger of this pair: not one byte of it runs
    /// while a level is being played, yet inlined it sat between the world
    /// loop and the HUD, which is the whole of what `draw_frame` does on the
    /// frames that matter. Outlining this and the intermission takes six and a
    /// half kilobytes out of that body for sixteen bytes of image.
    #[optimize(size)]
    #[inline(never)]
    fn draw_menu(&mut self, map: &ResidentMap, view: MenuView) {
        let packets = &mut self.menu_packets[self.arena];
        packets.clear();
        let image = TextureMaterial::opaque(clut_texture(), MENU_TPAGE, (0x80, 0x80, 0x80));
        packets.push(QuadTexturedMaterial::with_material(
            [(18, 48), (50, 48), (18, 192), (50, 192)],
            [
                (0, 0),
                (QPLAQUE_SIZE.0, 0),
                (0, QPLAQUE_SIZE.1),
                QPLAQUE_SIZE,
            ],
            image,
        ));

        push_centered_text(packets, 50, view.title(), (0x80, 0x80, 0x80));
        match view.page {
            MenuPage::Main => {
                for index in 0..view.row_count() {
                    if let Some(row) = view.row(index) {
                        push_shadowed_text(
                            packets,
                            84,
                            88 + i16::from(index) * 24,
                            row.label,
                            menu_row_color(index, view.selected),
                        );
                    }
                }
            }
            MenuPage::Levels => {
                // Ten rows at a 16-pixel pitch fit under the title. Plain
                // glyphs, not shadowed: ten titles would otherwise outrun the
                // menu packet capacity alongside the pause HUD.
                for index in 0..view.row_count() {
                    if let Some(row) = view.row(index) {
                        push_text(
                            packets,
                            78,
                            72 + i16::from(index) * 16,
                            row.label,
                            menu_row_color(index, view.selected),
                        );
                    }
                }
            }
            MenuPage::Options => {
                for index in 0..view.row_count() {
                    let Some(row) = view.row(index) else {
                        continue;
                    };
                    let y = options_row_top(view) + i16::from(index) * options_row_pitch(view);
                    // `M_Print` draws the original Options list without the
                    // console port's extra shadow. Besides matching it, plain
                    // glyphs leave room for both authentic 13-glyph sliders.
                    push_text(
                        packets,
                        76,
                        y,
                        row.label,
                        menu_row_color(index, view.selected),
                    );
                    if let Some(value) = row.value {
                        push_text(packets, 216, y, value, (0x80, 0x80, 0x80));
                    } else if index == OPTIONS_SOUND_VOLUME_ROW {
                        push_menu_slider(packets, 208, y, view.sound_volume);
                    } else if index == OPTIONS_MUSIC_VOLUME_ROW && view.music_available {
                        push_menu_slider(packets, 208, y, view.music_volume);
                    }
                }
            }
            MenuPage::Controls => {
                for (index, line) in CONTROL_LINES.iter().enumerate() {
                    push_shadowed_text(
                        packets,
                        66,
                        76 + index as i16 * 18,
                        line,
                        (0x80, 0x62, 0x38),
                    );
                }
                if let Some(row) = view.row(0) {
                    push_shadowed_text(packets, 84, 204, row.label, (0x80, 0x80, 0x80));
                }
            }
            MenuPage::Cheats => {
                for index in 0..view.row_count() {
                    let Some(row) = view.row(index) else {
                        continue;
                    };
                    let y = 92 + i16::from(index) * 24;
                    push_shadowed_text(
                        packets,
                        84,
                        y,
                        row.label,
                        menu_row_color(index, view.selected),
                    );
                    if let Some(value) = row.value {
                        push_shadowed_text(packets, 232, y, value, (0x80, 0x80, 0x80));
                    }
                }
            }
        }

        let row_y = match view.page {
            MenuPage::Main => 80 + i16::from(view.selected) * 24,
            MenuPage::Levels => 68 + i16::from(view.selected) * 16,
            MenuPage::Options => {
                options_row_top(view) - 8 + i16::from(view.selected) * options_row_pitch(view)
            }
            MenuPage::Controls => 196,
            MenuPage::Cheats => 84 + i16::from(view.selected) * 24,
        };
        const DOT_OFFSETS: [(i16, i16); 6] = [(0, 3), (2, 3), (4, 3), (6, 3), (4, 3), (3, 3)];
        let dot_frame = ((self.frame / 3) % 6) as usize;
        if let Some(dot) = map.picture_at(GraphicsPictureId::MenuDot1.index() + dot_frame) {
            let (offset_x, offset_y) = DOT_OFFSETS[dot_frame];
            push_picture(
                packets,
                60 + offset_x,
                row_y + offset_y,
                dot,
                (0x80, 0x80, 0x80),
            );
        }
        debug_assert!(packets.len() <= MENU_PACKET_CAPACITY);
        unsafe {
            crate::platform::register_screen_packets(
                packets.as_mut_ptr().cast::<u32>(),
                packets.len(),
                QuadTexturedMaterial::WORDS,
            );
        }
    }

    fn materialize_surface(
        &self,
        map: &ResidentMap,
        first: usize,
        flags: u16,
        light_styles: [u8; 2],
        texture: TextureInfo,
        output: &mut [ClassicAffineVertex],
    ) {
        let baked_uv = flags & FACE_BAKED_UV != 0;
        let baked_light = flags & FACE_BAKED_LIGHT != 0;
        let indexed = map.indexed_vertices().expect("validated PSB4 vertices");
        let corners = &indexed.corners[first..first + output.len()];
        unsafe {
            if baked_uv && baked_light {
                #[cfg(not(feature = "renderer-quake-baked-materialize"))]
                materialize_classic_affine_indexed_baked_vertices(
                    corners.as_ptr().cast::<ClassicAffineIndexedCorner>(),
                    indexed.positions.as_ptr().cast::<ClassicAffinePosition>(),
                    indexed.positions.len(),
                    output.len(),
                    output.as_mut_ptr(),
                );
                #[cfg(feature = "renderer-quake-baked-materialize")]
                materialize_quake_baked_inline(
                    corners.as_ptr().cast::<ClassicAffineIndexedCorner>(),
                    indexed.positions.as_ptr().cast::<ClassicAffinePosition>(),
                    indexed.positions.len(),
                    output.len(),
                    output.as_mut_ptr(),
                );
            } else {
                let style0 = self.light_styles[light_styles[0] as usize];
                let style1 = self.light_styles[light_styles[1] as usize];
                materialize_classic_affine_indexed_vertices(
                    corners.as_ptr().cast::<ClassicAffineIndexedCorner>(),
                    indexed.positions.as_ptr().cast::<ClassicAffinePosition>(),
                    indexed.positions.len(),
                    output.len(),
                    output.as_mut_ptr(),
                    [texture.atlas.x, texture.atlas.y],
                    [style0, style1],
                    baked_uv,
                    baked_light,
                );
            }
        }
    }

    #[cfg(feature = "renderer-indexed-projection")]
    fn materialize_indexed_world_face(
        &mut self,
        map: &ResidentMap,
        face: CookedDrawSurface,
        texture: TextureInfo,
        destination_first: usize,
        output: &mut [ClassicAffineVertex],
    ) {
        let flags = u16::from(face.flags);
        let baked_uv = flags & FACE_BAKED_UV != 0;
        let baked_light = flags & FACE_BAKED_LIGHT != 0;
        let indexed = map.indexed_vertices().expect("validated PSB4 vertices");
        let first = face.first_corner as usize;
        let corners = &indexed.corners[first..first + output.len()];
        unsafe {
            if baked_uv && baked_light {
                materialize_classic_affine_indexed_baked_vertices_with_projection_slots(
                    corners.as_ptr().cast::<ClassicAffineIndexedCorner>(),
                    indexed.positions.as_ptr().cast::<ClassicAffinePosition>(),
                    indexed.positions.len(),
                    output.len(),
                    output.as_mut_ptr(),
                    self.indexed_position_slots.as_mut_ptr(),
                    self.indexed_unique_positions.as_mut_ptr(),
                    &mut self.indexed_unique_count,
                    BATCH_MAX_VERTICES,
                    self.indexed_corner_slots
                        .as_mut_ptr()
                        .add(destination_first),
                );
            } else {
                let style0 = self.light_styles[face.light_styles[0] as usize];
                let style1 = self.light_styles[face.light_styles[1] as usize];
                materialize_classic_affine_indexed_vertices(
                    corners.as_ptr().cast::<ClassicAffineIndexedCorner>(),
                    indexed.positions.as_ptr().cast::<ClassicAffinePosition>(),
                    indexed.positions.len(),
                    output.len(),
                    output.as_mut_ptr(),
                    [texture.atlas.x, texture.atlas.y],
                    [style0, style1],
                    baked_uv,
                    baked_light,
                );
                collect_classic_affine_indexed_projection_slots(
                    corners.as_ptr().cast::<ClassicAffineIndexedCorner>(),
                    indexed.positions.len(),
                    output.len(),
                    self.indexed_position_slots.as_mut_ptr(),
                    self.indexed_unique_positions.as_mut_ptr(),
                    &mut self.indexed_unique_count,
                    BATCH_MAX_VERTICES,
                    self.indexed_corner_slots
                        .as_mut_ptr()
                        .add(destination_first),
                );
            }
        }
    }

    #[inline(always)]
    fn materialize_retained_face(
        &self,
        map: &ResidentMap,
        face: CookedDrawSurface,
        texture: TextureInfo,
        output: &mut [ClassicAffineVertex],
    ) {
        self.materialize_surface(
            map,
            face.first_corner as usize,
            u16::from(face.flags),
            face.light_styles,
            texture,
            output,
        );
    }

    #[cfg(feature = "renderer-hoisted-indexed-world")]
    #[inline(always)]
    fn materialize_retained_face_from_indexed(
        &self,
        indexed: IndexedVertices<'_>,
        face: CookedDrawSurface,
        texture: TextureInfo,
        output: &mut [ClassicAffineVertex],
    ) {
        let flags = u16::from(face.flags);
        let baked_uv = flags & FACE_BAKED_UV != 0;
        let baked_light = flags & FACE_BAKED_LIGHT != 0;
        let first = face.first_corner as usize;
        let corners = &indexed.corners[first..first + output.len()];
        unsafe {
            if baked_uv && baked_light {
                materialize_classic_affine_indexed_baked_vertices(
                    corners.as_ptr().cast::<ClassicAffineIndexedCorner>(),
                    indexed.positions.as_ptr().cast::<ClassicAffinePosition>(),
                    indexed.positions.len(),
                    output.len(),
                    output.as_mut_ptr(),
                );
            } else {
                let style0 = self.light_styles[face.light_styles[0] as usize];
                let style1 = self.light_styles[face.light_styles[1] as usize];
                materialize_classic_affine_indexed_vertices(
                    corners.as_ptr().cast::<ClassicAffineIndexedCorner>(),
                    indexed.positions.as_ptr().cast::<ClassicAffinePosition>(),
                    indexed.positions.len(),
                    output.len(),
                    output.as_mut_ptr(),
                    [texture.atlas.x, texture.atlas.y],
                    [style0, style1],
                    baked_uv,
                    baked_light,
                );
            }
        }
    }

    #[inline(always)]
    fn materialize_face(
        &self,
        map: &ResidentMap,
        face: Face,
        texture: TextureInfo,
        output: &mut [ClassicAffineVertex],
    ) {
        self.materialize_surface(
            map,
            face.first_vertex as usize,
            face.flags,
            face.light_styles,
            texture,
            output,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_entities(
        &mut self,
        map: &ResidentMap,
        entities: &[RenderEntity],
        rotating_yaw: i16,
        camera: Camera,
        view: QuakeViewTransform,
        mut next: *mut u32,
        end: *mut u32,
        stats: &mut RenderStats,
    ) -> *mut u32 {
        self.visible_entity_indices.clear();
        let frustum = self.frustum(camera);
        scene::load_aabb_clip4(&frustum);
        for (index, entity) in entities.iter().enumerate() {
            if !entity.visible {
                continue;
            }
            if entity.is_projectile() {
                stats.projectile_entities = stats.projectile_entities.saturating_add(1);
            }
            if !self.point_visible(entity.leaf_index as usize) {
                continue;
            }
            if entity.is_projectile() {
                stats.pvs_projectile_entities = stats.pvs_projectile_entities.saturating_add(1);
            }
            if !scene::aabb_outside_clip4(entity.clip_mins, entity.clip_maxs, &frustum, 0x0f) {
                if self.visible_entity_indices.len() == self.visible_entity_indices.capacity() {
                    stats.packet_overflow_avoided = true;
                    break;
                }
                self.visible_entity_indices.push(index as u16);
                if entity.is_projectile() {
                    stats.visible_projectile_entities =
                        stats.visible_projectile_entities.saturating_add(1);
                }
            }
        }

        let models = map.alias_models();
        for visible in 0..self.visible_entity_indices.len() {
            let entity = &entities[self.visible_entity_indices[visible] as usize];
            if entity.model_id < 0 {
                // The all-map route regression validates map data and real
                // transition volumes under a fixed cycle budget. Shipping
                // and dedicated visual captures still render every submodel.
                #[cfg(feature = "episode1-regression")]
                {
                    continue;
                }
                #[cfg(not(feature = "episode1-regression"))]
                {
                    next = self.draw_brush_entity(map, entity, camera, view, next, end, stats);
                    if stats.packet_overflow_avoided {
                        break;
                    }
                    stats.visible_entities = stats.visible_entities.saturating_add(1);
                    continue;
                }
            }
            let Some(model) = models.model_at(entity.model_index as usize) else {
                continue;
            };
            debug_assert_eq!(model.header().id, entity.model_id);
            let header = model.header();
            if alias_model_is_sprite(header) {
                let submitted = draw_sprite_model(
                    model,
                    entity.frame as usize,
                    entity.origin,
                    [entity.angles.x, entity.angles.y, entity.angles.z],
                    camera,
                    view,
                    next,
                    end,
                );
                if submitted.overflow {
                    stats.packet_overflow_avoided = true;
                    break;
                }
                next = submitted.next;
                stats.visible_entities = stats.visible_entities.saturating_add(1);
                if submitted.drawn {
                    stats.sprite_packets = stats.sprite_packets.saturating_add(1);
                    stats.packets = stats.packets.saturating_add(1);
                    stats.hardware_triangles = stats.hardware_triangles.saturating_add(2);
                }
                continue;
            }
            let face_count = header.triangle_count as usize;
            let Some(worst_words) = face_count.checked_mul(ALIAS_PACKET_WORDS) else {
                stats.packet_overflow_avoided = true;
                break;
            };
            if !packet_capacity(next, end, worst_words) {
                stats.packet_overflow_avoided = true;
                break;
            }

            let frame = (entity.frame as usize).min(header.frame_count as usize - 1);
            let skin = (entity.skin as usize).min(header.skin_count as usize - 1);
            let vertices = model
                .frame_bytes(frame)
                .expect("validated alias-model frame");
            let faces = model
                .triangle_bytes(skin)
                .expect("validated alias-model skin");
            debug_assert_eq!(vertices.len(), header.vertex_count as usize * 3);
            debug_assert_eq!(
                faces.len(),
                face_count * core::mem::size_of::<ClassicAliasFace>()
            );
            debug_assert_eq!(faces.as_ptr() as usize & 3, 0);

            let yaw = if model_rotates(header) {
                rotating_yaw
            } else {
                entity.angles.y
            };
            let model_rotation = Mat3I16::rotate_z((yaw as u16) >> 4)
                .mul(&Mat3I16::rotate_y((entity.angles.x as u16) >> 4));
            let (rotation, translation) = compose_classic_alias_transform(
                view.rotation,
                view.translation,
                model_rotation,
                GteVec3I16::new(header.offset.x, header.offset.y, header.offset.z),
                GteVec3I32::new(
                    entity.origin.x >> 12,
                    entity.origin.y >> 12,
                    entity.origin.z >> 12,
                ),
                GteVec3I16::new(header.scale.x, header.scale.y, header.scale.z),
            );
            scene::load_rotation(&rotation);
            scene::load_translation(translation);
            let light =
                (i32::from(entity.light) + self.dynamic_light_at(entity.origin)).min(255) as u32;
            let tint = light | (light << 8) | (light << 16);
            let submitted = unsafe {
                submit_classic_alias_model(
                    vertices.as_ptr().cast::<ClassicAliasVertex>(),
                    header.vertex_count as usize,
                    faces.as_ptr().cast::<ClassicAliasFace>(),
                    face_count,
                    self.alias_projected.as_mut_ptr(),
                    next,
                    header.skins[skin].texture_page,
                    clut_texture(),
                    tint,
                    ClassicAffineProfile::QUAKE_REFERENCE,
                )
            };
            next = submitted.next_packet;
            stats.visible_entities = stats.visible_entities.saturating_add(1);
            stats.alias_packets = stats.alias_packets.wrapping_add(submitted.packets);
            if entity.is_projectile() {
                stats.projectile_packets = stats.projectile_packets.wrapping_add(submitted.packets);
                match entity.model_id {
                    quake_core::combat::NAIL_MODEL_ID => {
                        stats.nail_projectile_packets = stats
                            .nail_projectile_packets
                            .wrapping_add(submitted.packets);
                    }
                    quake_core::combat::GRENADE_MODEL_ID => {
                        stats.grenade_projectile_packets = stats
                            .grenade_projectile_packets
                            .wrapping_add(submitted.packets);
                    }
                    quake_core::combat::ROCKET_MODEL_ID => {
                        stats.rocket_projectile_packets = stats
                            .rocket_projectile_packets
                            .wrapping_add(submitted.packets);
                    }
                    _ => {}
                }
            }
            stats.packets = stats.packets.wrapping_add(submitted.packets);
            stats.hardware_triangles = stats
                .hardware_triangles
                .wrapping_add(submitted.hardware_triangles);
        }
        next
    }

    fn frustum(&mut self, camera: Camera) -> [AabbClipPlane; 4] {
        if let Some((cached_camera, cached_frustum)) = self.cached_frustum {
            if cached_camera == camera {
                cached_frustum
            } else {
                let frustum = quake_frustum(camera);
                self.cached_frustum = Some((camera, frustum));
                frustum
            }
        } else {
            let frustum = quake_frustum(camera);
            self.cached_frustum = Some((camera, frustum));
            frustum
        }
    }

    fn draw_view_model(
        &mut self,
        map: &ResidentMap,
        camera: Camera,
        input: ViewModelInput,
        output: *mut u32,
        end: *mut u32,
        stats: &mut RenderStats,
    ) {
        let models = map.alias_models();
        let Some(model) = (0..models.len()).find_map(|index| {
            let model = models.model_at(index)?;
            (model.header().id == input.weapon.model_id).then_some(model)
        }) else {
            stats.packet_overflow_avoided = true;
            return;
        };
        let header = model.header();
        let face_count = header.triangle_count as usize;
        let Some(worst_words) = face_count.checked_mul(ALIAS_PACKET_WORDS) else {
            stats.packet_overflow_avoided = true;
            return;
        };
        if face_count > VIEW_MODEL_PACKET_CAPACITY || !packet_capacity(output, end, worst_words) {
            stats.packet_overflow_avoided = true;
            return;
        }
        let frame = (input.weapon.frame as usize).min(header.frame_count as usize - 1);
        let vertices = model
            .frame_bytes(frame)
            .expect("validated view-model frame");
        let faces = model.triangle_bytes(0).expect("validated view-model skin");

        // The retained renderer magnifies alias view models by 2^3 and uses
        // only Quake's coordinate basis, not the player's world-facing view
        // rotation. This keeps the weapon camera-relative while preserving
        // the authored v_shot.mdl framing. Screen packets are registered after
        // the world OT so walls can never obscure the weapon.
        let (bob_phase, bob_x) = view_model::advance_bob(
            self.view_model_bob_phase,
            input.velocity.x,
            input.velocity.y,
            input.elapsed_ticks,
        );
        self.view_model_bob_phase = bob_phase;
        let (rotation, translation) = compose_classic_alias_transform(
            crate::platform::quake_coordinate_rotation(),
            GteVec3I32::ZERO,
            Mat3I16::IDENTITY,
            GteVec3I16::new(
                view_model::magnify_x_with_bob(
                    header.offset.x,
                    bob_x.saturating_add(view_model::bob_forward(input.bob_q12)),
                ),
                view_model::magnify_component(header.offset.y),
                view_model::magnify_component(header.offset.z),
            ),
            GteVec3I32::ZERO,
            GteVec3I16::new(
                view_model::magnify_component(header.scale.x),
                view_model::magnify_component(header.scale.y),
                view_model::magnify_component(header.scale.z),
            ),
        );
        scene::load_rotation(&rotation);
        scene::load_translation(translation);
        let camera_light = map
            .point_leaf_index(camera.origin)
            .and_then(|index| map.leaves().get(index))
            .map(|leaf| {
                quake_core::lightstyle::sample_leaf(
                    leaf.lightmap,
                    leaf.light_styles,
                    &self.light_styles,
                )
            })
            .unwrap_or(0);
        self.view_model_light =
            view_model::update_light(self.view_model_light, camera_light, input.muzzle_flash);
        let submitted = unsafe {
            submit_classic_alias_view_model(
                vertices.as_ptr().cast::<ClassicAliasVertex>(),
                header.vertex_count as usize,
                faces.as_ptr().cast::<ClassicAliasFace>(),
                face_count,
                self.alias_projected.as_mut_ptr(),
                output,
                header.skins[0].texture_page,
                view_model::CLUT,
                view_model::packet_tint(self.view_model_light),
                ClassicAffineProfile::QUAKE_REFERENCE,
            )
        };
        let _registered = unsafe {
            crate::platform::register_screen_packets(
                output,
                submitted.packets as usize,
                ClassicTriTextured::WORDS,
            )
        };
        #[cfg(feature = "visual-parity-regression")]
        {
            stats.view_model_registered_packets = _registered as u32;
        }
        stats.alias_packets = stats.alias_packets.wrapping_add(submitted.packets);
        stats.view_model_packets = submitted.packets;
        stats.packets = stats.packets.wrapping_add(submitted.packets);
        stats.hardware_triangles = stats
            .hardware_triangles
            .wrapping_add(submitted.hardware_triangles);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_brush_entity(
        &self,
        map: &ResidentMap,
        entity: &RenderEntity,
        camera: Camera,
        view: QuakeViewTransform,
        mut next: *mut u32,
        end: *mut u32,
        stats: &mut RenderStats,
    ) -> *mut u32 {
        let Some(model) = map.brush_models().get(entity.model_index as usize) else {
            return next;
        };
        let (rotation, translation) = compose_classic_alias_transform(
            view.rotation,
            view.translation,
            Mat3I16::IDENTITY,
            GteVec3I16::ZERO,
            GteVec3I32::new(
                entity.origin.x >> 12,
                entity.origin.y >> 12,
                entity.origin.z >> 12,
            ),
            GteVec3I16::new(0x1000, 0x1000, 0x1000),
        );
        scene::load_rotation(&rotation);
        scene::load_translation(translation);

        let model_camera = Vec3I32 {
            x: camera.origin.x.saturating_sub(entity.origin.x),
            y: camera.origin.y.saturating_sub(entity.origin.y),
            z: camera.origin.z.saturating_sub(entity.origin.z),
        };
        // The whole model reaches behind the near plane or none of it does.
        let near = NearPlane::new(camera).reaches_behind(entity.clip_mins, entity.clip_maxs);
        let batch_vertices = scratchpad_batch_vertices();
        let mut batch_surfaces = uninit_batch_surfaces();
        let mut batch_vertex_count = 0usize;
        let mut batch_surface_count = 0usize;
        let mut batch_worst_words = 0usize;
        let first_face = model.first_face as usize;
        let face_end = first_face.saturating_add(model.face_count as usize);
        for face_index in first_face..face_end {
            let Some(face) = map.faces().get(face_index) else {
                stats.packet_overflow_avoided = true;
                break;
            };
            let Some(&texture) = self.active_textures.get(face.texture as usize) else {
                stats.packet_overflow_avoided = true;
                break;
            };
            if texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL | TEXTURE_SKY) != 0
                || !front_facing(map, face, model_camera)
            {
                continue;
            }
            let vertex_count = face.vertex_count as usize;
            let clip = near && vertex_count < NEAR_CLIP_MAX_VERTICES;
            let reserve_count = vertex_count + usize::from(clip);
            let windowed = texture.flags & TEXTURE_LIQUID != 0;
            let face_worst_words = (reserve_count - 2)
                * if windowed {
                    WORST_WINDOWED_PACKET_WORDS_PER_TRIANGLE
                } else {
                    WORST_PACKET_WORDS_PER_TRIANGLE
                };
            if windowed
                || batch_vertex_count + reserve_count > BATCH_MAX_VERTICES
                || batch_surface_count == BATCH_MAX_SURFACES
                || !packet_capacity(next, end, batch_worst_words + face_worst_words)
            {
                let submitted = unsafe {
                    flush_batch(
                        batch_vertices.as_mut_ptr().cast(),
                        batch_vertex_count,
                        batch_surfaces.as_ptr().cast(),
                        batch_surface_count,
                        next,
                    )
                };
                next = submitted.next_packet;
                stats.packets = stats.packets.wrapping_add(submitted.packets);
                stats.hardware_triangles = stats
                    .hardware_triangles
                    .wrapping_add(submitted.hardware_triangles);
                batch_vertex_count = 0;
                batch_surface_count = 0;
                batch_worst_words = 0;
            }
            if !packet_capacity(next, end, face_worst_words) {
                stats.packet_overflow_avoided = true;
                break;
            }
            if windowed {
                let vertices = unsafe { batch_vertices_mut(batch_vertices, 0, vertex_count) };
                self.materialize_face(map, face, texture, vertices);
                animate_special_surface(vertices, texture, self.frame);
                let vertex_count = if clip {
                    unsafe { clip_face_near(batch_vertices.as_mut_ptr().cast(), vertex_count) }
                } else {
                    vertex_count
                };
                if vertex_count < 3 {
                    continue;
                }
                #[cfg(feature = "renderer-window-range-coalescing")]
                let window_packet_start = next;
                let submitted = unsafe {
                    submit_classic_affine_scoped_windowed_fan(
                        batch_vertices.as_mut_ptr().cast(),
                        vertex_count,
                        next,
                        texture.texture_page,
                        clut_texture(),
                        special_texture_window(texture).word(),
                        ClassicAffineProfile::QUAKE_REFERENCE,
                    )
                };
                #[cfg(feature = "renderer-window-range-coalescing")]
                unsafe {
                    crate::platform::register_world_window_packet_range(
                        window_packet_start,
                        submitted.next_packet,
                    );
                }
                next = submitted.next_packet;
                stats.packets = stats.packets.wrapping_add(submitted.packets);
                stats.hardware_triangles = stats
                    .hardware_triangles
                    .wrapping_add(submitted.hardware_triangles);
            } else {
                let vertices = unsafe {
                    batch_vertices_mut(batch_vertices, batch_vertex_count, reserve_count)
                };
                self.materialize_face(map, face, texture, &mut vertices[..vertex_count]);
                let vertex_count = if clip {
                    unsafe { clip_face_near(vertices.as_mut_ptr(), vertex_count) }
                } else {
                    vertex_count
                };
                if vertex_count < 3 {
                    continue;
                }
                batch_surfaces[batch_surface_count].write(ClassicAffineBatchSurface {
                    first_vertex: batch_vertex_count as u16,
                    vertex_count: vertex_count as u16,
                    tpage: texture.texture_page,
                    clut: clut_texture(),
                });
                batch_vertex_count += vertex_count;
                batch_surface_count += 1;
                batch_worst_words += (vertex_count - 2) * WORST_PACKET_WORDS_PER_TRIANGLE;
            }
            stats.visible_faces = stats.visible_faces.saturating_add(1);
        }
        let submitted = unsafe {
            flush_batch(
                batch_vertices.as_mut_ptr().cast(),
                batch_vertex_count,
                batch_surfaces.as_ptr().cast(),
                batch_surface_count,
                next,
            )
        };
        stats.packets = stats.packets.wrapping_add(submitted.packets);
        stats.hardware_triangles = stats
            .hardware_triangles
            .wrapping_add(submitted.hardware_triangles);
        submitted.next_packet
    }

    fn point_visible(&self, leaf_index: usize) -> bool {
        if leaf_index == 0 {
            return false;
        }
        let visible_index = leaf_index - 1;
        visible_index < self.visible_leaf_count
            && self.visibility[visible_index >> 3] & (1 << (visible_index & 7)) != 0
    }

    /// Build the ordinary camera PVS, then optionally open one water boundary
    /// already present in it into exactly one opposite PVS. A failed union is
    /// rebuilt opaque immediately, so custom maps cannot turn this option into
    /// an unbounded face-cache allocation or a partially rendered frame.
    #[optimize(size)]
    #[inline(never)]
    fn prepare_visibility(&mut self, map: &ResidentMap, camera: Camera, water_alpha: bool) -> bool {
        let camera_leaf = map.point_leaf_index(camera.origin);
        let camera_matches = camera_leaf.is_some_and(|leaf| {
            self.cached_visibility
                .is_some_and(|(generation, cached_leaf, _)| {
                    generation == map.generation() && cached_leaf == leaf
                })
        });
        if water_alpha {
            if !camera_matches && !self.mark_visible_faces(map, camera.origin, None) {
                return false;
            }
            if let Some(portal) = self.water_portal(map, camera.origin) {
                if self.mark_visible_faces(map, camera.origin, Some(portal.leaf)) {
                    self.active_water_plane = portal.plane;
                    return true;
                }
            }
        }

        self.active_water_plane = -1;
        self.mark_visible_faces(map, camera.origin, None)
    }

    /// Rebuild the conservative 16-face unions only when the PVS list changes.
    /// The per-frame selector can then reject a whole block with the same
    /// exact four-plane test used for an individual face.
    #[cfg(feature = "renderer-block-frustum")]
    #[inline(never)]
    fn rebuild_visible_face_blocks(&mut self) {
        self.visible_face_blocks.clear();
        let mut first = 0usize;
        while first < self.visible_faces.len() {
            let end = (first + VISIBLE_FACE_BLOCK_SIZE).min(self.visible_faces.len());
            let mut mins = self.visible_faces[first].bounds.mins;
            let mut maxs = self.visible_faces[first].bounds.maxs;
            let mut index = first + 1;
            while index < end {
                let bounds = self.visible_faces[index].bounds;
                let mut axis = 0usize;
                while axis < 3 {
                    mins[axis] = mins[axis].min(bounds.mins[axis]);
                    maxs[axis] = maxs[axis].max(bounds.maxs[axis]);
                    axis += 1;
                }
                index += 1;
            }
            self.visible_face_blocks
                .push(VisibleFaceBlock { mins, maxs });
            first = end;
        }
        debug_assert_eq!(
            self.visible_face_blocks.len(),
            self.visible_faces.len().div_ceil(VISIBLE_FACE_BLOCK_SIZE)
        );

        #[cfg(feature = "renderer-hierarchical-block-frustum")]
        {
            self.visible_face_super_blocks.clear();
            for group in self
                .visible_face_blocks
                .chunks(VISIBLE_FACE_SUPER_BLOCK_SIZE)
            {
                let mut mins = group[0].mins;
                let mut maxs = group[0].maxs;
                for block in &group[1..] {
                    let mut axis = 0usize;
                    while axis < 3 {
                        mins[axis] = mins[axis].min(block.mins[axis]);
                        maxs[axis] = maxs[axis].max(block.maxs[axis]);
                        axis += 1;
                    }
                }
                self.visible_face_super_blocks
                    .push(VisibleFaceBlock { mins, maxs });
            }
            debug_assert_eq!(
                self.visible_face_super_blocks.len(),
                self.visible_face_blocks
                    .len()
                    .div_ceil(VISIBLE_FACE_SUPER_BLOCK_SIZE)
            );
        }
    }

    /// Locate a PVS-resident water/empty boundary. Sampling eight units on both
    /// sides of the plane identifies the opposite BSP leaf without retaining
    /// new topology; only that one PVS is ever merged into the frame.
    #[optimize(size)]
    #[inline(never)]
    fn water_portal(&self, map: &ResidentMap, point: Vec3I32) -> Option<WaterPortal> {
        let camera_leaf = map.point_leaf_index(point)?;
        let camera_contents = map.leaves().get(camera_leaf)?.contents;
        if camera_contents != CONTENTS_EMPTY && camera_contents != CONTENTS_WATER {
            return None;
        }
        for (visible_index, visible) in self.visible_faces.iter().enumerate() {
            #[cfg(not(feature = "renderer-compact-cell-stream"))]
            let _ = visible_index;
            let Some(texture) = self.active_textures.get(visible.face.material as usize) else {
                continue;
            };
            if texture.flags & TEXTURE_LIQUID == 0 {
                continue;
            }
            #[cfg(feature = "renderer-compact-cell-stream")]
            let plane = unsafe { *self.visible_face_planes.get_unchecked(visible_index) };
            #[cfg(not(feature = "renderer-compact-cell-stream"))]
            let plane = visible.plane;
            let center = water_face_sample(map, visible.face);
            // Axial cooked planes intentionally use `kind` as their hot
            // normal; their retained normal components are not authoritative.
            // Match `plane_distance` or both samples can remain in one leaf.
            let mut step = Vec3I32 { x: 0, y: 0, z: 0 };
            match plane.kind {
                0 => step.x = 8 << 12,
                1 => step.y = 8 << 12,
                2 => step.z = 8 << 12,
                _ => {
                    step.x = mul_q12_i32(8 << 12, i32::from(plane.normal.x));
                    step.y = mul_q12_i32(8 << 12, i32::from(plane.normal.y));
                    step.z = mul_q12_i32(8 << 12, i32::from(plane.normal.z));
                }
            }
            let Some(positive) = map.point_leaf_index(Vec3I32 {
                x: center.x.wrapping_add(step.x),
                y: center.y.wrapping_add(step.y),
                z: center.z.wrapping_add(step.z),
            }) else {
                continue;
            };
            let Some(negative) = map.point_leaf_index(Vec3I32 {
                x: center.x.wrapping_sub(step.x),
                y: center.y.wrapping_sub(step.y),
                z: center.z.wrapping_sub(step.z),
            }) else {
                continue;
            };
            let Some(positive_contents) = map.leaves().get(positive).map(|leaf| leaf.contents)
            else {
                continue;
            };
            let Some(negative_contents) = map.leaves().get(negative).map(|leaf| leaf.contents)
            else {
                continue;
            };
            let opposite = if positive_contents == camera_contents
                && negative_contents != camera_contents
            {
                (negative, negative_contents)
            } else if negative_contents == camera_contents && positive_contents != camera_contents {
                (positive, positive_contents)
            } else {
                continue;
            };
            if !matches!(
                (camera_contents, opposite.1),
                (CONTENTS_EMPTY, CONTENTS_WATER) | (CONTENTS_WATER, CONTENTS_EMPTY)
            ) || opposite.0 > u16::MAX as usize
            {
                continue;
            }
            return Some(WaterPortal {
                plane: visible.face.plane as i16,
                leaf: opposite.0 as u16,
            });
        }
        None
    }

    #[optimize(size)]
    #[inline(never)]
    fn mark_visible_faces(
        &mut self,
        map: &ResidentMap,
        point: Vec3I32,
        portal_leaf: Option<u16>,
    ) -> bool {
        #[cfg(feature = "renderer-compact-cell-stream")]
        if self.visible_face_planes.len() != self.visible_faces.len() {
            self.visible_face_planes.clear();
            self.visible_faces.clear();
        }
        let faces = map.faces();
        if faces.len() > self.face_visible.len() * 4 {
            self.visible_faces.clear();
            return false;
        }
        let Some(leaf_index) = map.point_leaf_index(point) else {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            self.visible_faces.clear();
            return false;
        };
        if leaf_index == 0 {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            self.visible_faces.clear();
            return false;
        }
        let portal_key = portal_leaf.unwrap_or(u16::MAX);
        if self.cached_visibility == Some((map.generation(), leaf_index, portal_key)) {
            return true;
        }
        // Entries decoded from another map cannot be carried over.
        if self.visible_faces_generation != Some(map.generation()) {
            self.visible_faces.clear();
            #[cfg(feature = "renderer-compact-cell-stream")]
            self.visible_face_planes.clear();
            self.visible_faces_generation = Some(map.generation());
        }
        let face_words = faces.len().div_ceil(4);
        self.face_visible[..face_words].fill(0);
        let leaf = map.leaves().get(leaf_index).expect("validated leaf");
        if leaf.visibility_offset < 0 {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            self.visible_faces.clear();
            return false;
        }

        let world = map.brush_models().get(0).expect("validated world model");
        let visible_leaves = world.visible_leaves.max(0) as usize;
        let row_bytes = (visible_leaves + 7) >> 3;
        if row_bytes > self.visibility.len() {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            self.visible_faces.clear();
            return false;
        }
        self.visibility.fill(0);
        if !decompress_visibility(
            map.visibility(),
            leaf.visibility_offset as usize,
            &mut self.visibility[..row_bytes],
        ) {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            self.visible_faces.clear();
            return false;
        }
        if let Some(portal_leaf) = portal_leaf {
            let Some(portal) = map.leaves().get(portal_leaf as usize) else {
                self.cached_visibility = None;
                return false;
            };
            if portal.visibility_offset < 0
                || !merge_visibility(
                    map.visibility(),
                    portal.visibility_offset as usize,
                    &mut self.visibility[..row_bytes],
                )
            {
                self.cached_visibility = None;
                return false;
            }
        }

        let leaves = map.leaves();
        let marks = map.mark_surfaces();
        let face_marks = self.face_visible.as_mut_ptr().cast::<u8>();
        for visible_index in 0..visible_leaves {
            if self.visibility[visible_index >> 3] & (1 << (visible_index & 7)) == 0 {
                continue;
            }
            let Some(leaf) = leaves.get(visible_index + 1) else {
                self.visible_faces.clear();
                return false;
            };
            let start = leaf.first_mark_surface as usize;
            let end = start + leaf.mark_surface_count as usize;
            for mark_index in start..end {
                let face = marks.get(mark_index).expect("validated mark surface") as usize;
                // Validated at load: every mark surface names a face.
                debug_assert!(face < faces.len());
                unsafe {
                    ptr::write(face_marks.add(face), 1);
                }
            }
        }

        // The list is the marked faces in ascending order, each decoded once.
        // Consecutive leaves see mostly the same faces, so entries already in
        // the list are carried over: drop the ones no longer marked (an
        // in-place compaction), then merge the newly marked faces in from the
        // back, decoding only those. The result is exactly the list a full
        // ascending scan would build.
        let mut kept = 0usize;
        for old_index in 0..self.visible_faces.len() {
            let mut face_index = unsafe {
                self.visible_faces
                    .get_unchecked(old_index)
                    .bounds
                    .surface_index
            };
            #[cfg(any(
                feature = "renderer-compact-cell-stream",
                feature = "renderer-cell-policy"
            ))]
            {
                face_index &= VISIBLE_SURFACE_INDEX_MASK;
            }
            if unsafe { ptr::read(face_marks.add(face_index as usize)) } != 0 {
                if kept != old_index {
                    let entries = self.visible_faces.as_mut_ptr();
                    unsafe { move_visible_face(entries.add(old_index), entries.add(kept)) };
                    #[cfg(feature = "renderer-compact-cell-stream")]
                    unsafe {
                        let planes = self.visible_face_planes.as_mut_ptr();
                        ptr::write(planes.add(kept), ptr::read(planes.add(old_index)));
                    }
                }
                kept += 1;
            }
        }
        unsafe { self.visible_faces.set_len(kept) };
        #[cfg(feature = "renderer-compact-cell-stream")]
        unsafe {
            self.visible_face_planes.set_len(kept)
        };

        // Newly marked faces, ascending, in the frame index scratch (free
        // until `draw_frame` refills it after this call). A word of four
        // unmarked faces is skipped in one load.
        self.frame_face_indices.clear();
        let new_faces = self.frame_face_indices.as_mut_ptr();
        let new_capacity = self.frame_face_indices.capacity();
        let mut new_count = 0usize;
        let mut face_index = 0usize;
        let mut kept_cursor = 0usize;
        while face_index < faces.len() {
            let word = unsafe { *self.face_visible.get_unchecked(face_index >> 2) };
            if word == 0 {
                face_index += 4;
                continue;
            }
            let last = (face_index + 4).min(faces.len());
            while face_index < last {
                if unsafe { ptr::read(face_marks.add(face_index)) } != 0 {
                    // Kept entries are ascending, so the next one either is
                    // this face (carried over) or lies beyond it (new face).
                    let mut kept_face = u16::MAX;
                    if kept_cursor < kept {
                        kept_face = unsafe {
                            self.visible_faces
                                .get_unchecked(kept_cursor)
                                .bounds
                                .surface_index
                        };
                        #[cfg(any(
                            feature = "renderer-compact-cell-stream",
                            feature = "renderer-cell-policy"
                        ))]
                        {
                            kept_face &= VISIBLE_SURFACE_INDEX_MASK;
                        }
                    }
                    if kept_cursor < kept && kept_face as usize == face_index {
                        kept_cursor += 1;
                    } else {
                        // Same limit as pushing past the list's capacity:
                        // every kept entry and every new face ends up in it.
                        #[cfg(feature = "renderer-compact-cell-stream")]
                        let plane_full =
                            kept + new_count == self.visible_face_planes.capacity();
                        #[cfg(not(feature = "renderer-compact-cell-stream"))]
                        let plane_full = false;
                        if kept + new_count == self.visible_faces.capacity()
                            || plane_full
                            || new_count == new_capacity
                        {
                            self.visible_faces.clear();
                            self.cached_visibility = None;
                            self.visible_leaf_count = 0;
                            return false;
                        }
                        unsafe { ptr::write(new_faces.add(new_count), face_index as u16) };
                        new_count += 1;
                    }
                }
                face_index += 1;
            }
        }
        debug_assert_eq!(kept_cursor, kept);
        let total = kept + new_count;
        debug_assert!(total <= self.visible_faces.capacity());

        // Backward merge: the write cursor never passes the read cursor
        // because everything still to be placed lies at or below it.
        unsafe { self.visible_faces.set_len(total) };
        #[cfg(feature = "renderer-compact-cell-stream")]
        unsafe {
            self.visible_face_planes.set_len(total)
        };
        let entries = self.visible_faces.as_mut_ptr();
        #[cfg(feature = "renderer-compact-cell-stream")]
        let plane_entries = self.visible_face_planes.as_mut_ptr();
        let mut write = total;
        let mut read = kept;
        let mut new_index = new_count;
        while new_index > 0 {
            let new_face = unsafe { ptr::read(new_faces.add(new_index - 1)) } as usize;
            while read > 0 && {
                let mut old_face = unsafe { (*entries.add(read - 1)).bounds.surface_index };
                #[cfg(any(
                    feature = "renderer-compact-cell-stream",
                    feature = "renderer-cell-policy"
                ))]
                {
                    old_face &= VISIBLE_SURFACE_INDEX_MASK;
                }
                old_face as usize > new_face
            } {
                write -= 1;
                read -= 1;
                if write != read {
                    unsafe { move_visible_face(entries.add(read), entries.add(write)) };
                    #[cfg(feature = "renderer-compact-cell-stream")]
                    unsafe {
                        ptr::write(
                            plane_entries.add(write),
                            ptr::read(plane_entries.add(read)),
                        );
                    }
                }
            }
            write -= 1;
            let face = faces.get(new_face).expect("validated face");
            let plane = map
                .planes()
                .get(face.plane as usize)
                .expect("validated face plane");
            let (mins, maxs) = face_bounds(map, face);
            let mut sign_bits = 0u8;
            for (axis, normal) in [plane.normal.x, plane.normal.y, plane.normal.z]
                .into_iter()
                .enumerate()
            {
                if normal < 0 {
                    sign_bits |= 1 << axis;
                }
            }
            let compact_plane = CompactPlane {
                normal: plane.normal,
                kind: plane.kind as u8,
                sign_bits,
                distance: plane.distance,
            };
            unsafe {
                ptr::write(
                    entries.add(write),
                    VisibleFace {
                        face: CookedDrawSurface {
                            plane: face.plane as u16,
                            first_corner: face.first_vertex as u16,
                            material: face.texture as u16,
                            flags: face.flags as u8,
                            corner_count: face.vertex_count as u8,
                            light_styles: face.light_styles,
                        },
                        #[cfg(not(feature = "renderer-compact-cell-stream"))]
                        plane: compact_plane,
                        bounds: RetainedSurfaceBounds {
                            surface_index: new_face as u16,
                            mins,
                            maxs,
                        },
                    },
                );
                #[cfg(feature = "renderer-compact-cell-stream")]
                ptr::write(plane_entries.add(write), compact_plane);
            };
            new_index -= 1;
        }
        debug_assert_eq!(write, read);
        #[cfg(any(
            feature = "renderer-compact-cell-stream",
            feature = "renderer-cell-policy"
        ))]
        self.retain_cell_faces(map, leaf_index);
        #[cfg(feature = "renderer-block-frustum")]
        self.rebuild_visible_face_blocks();
        self.visible_leaf_count = visible_leaves;
        self.cached_visibility = Some((map.generation(), leaf_index, portal_key));
        true
    }

    /// Compile the freshly merged PVS into the camera cell's retained stream.
    /// Ordinary invariant backs disappear here; invariant fronts keep no hot
    /// dependency on their parallel plane record. Liquids remain dynamic so
    /// the opposite-PVS water override retains its established semantics.
    #[cfg(any(
        feature = "renderer-compact-cell-stream",
        feature = "renderer-cell-policy"
    ))]
    #[inline(never)]
    fn retain_cell_faces(&mut self, map: &ResidentMap, leaf_index: usize) {
        let Some(bounds) = map.leaf_bounds(leaf_index) else {
            return;
        };
        let textures = map.render_textures();
        let faces = self.visible_faces.as_mut_ptr();
        #[cfg(feature = "renderer-compact-cell-stream")]
        let planes = self.visible_face_planes.as_mut_ptr();
        let mut write = 0usize;
        for read in 0..self.visible_faces.len() {
            let visible = unsafe { &mut *faces.add(read) };
            visible.bounds.surface_index &= VISIBLE_SURFACE_INDEX_MASK;
            let texture = unsafe { textures.get_unchecked(visible.face.material as usize) };
            if texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) != 0 {
                continue;
            }
            #[cfg(feature = "renderer-compact-cell-stream")]
            let plane = unsafe { ptr::read(planes.add(read)) };
            #[cfg(all(
                feature = "renderer-cell-policy",
                not(feature = "renderer-compact-cell-stream")
            ))]
            let plane = visible.plane;
            #[cfg(feature = "renderer-cell-liquid-policy")]
            if texture.flags & TEXTURE_LIQUID != 0 {
                visible.bounds.surface_index |= VISIBLE_LIQUID_BIT;
            } else {
                match leaf_invariant_facing(plane, u16::from(visible.face.flags), bounds) {
                    Some(false) => continue,
                    Some(true) => {
                        visible.bounds.surface_index |= VISIBLE_INVARIANT_FRONT_BIT;
                    }
                    None => {}
                }
            }
            #[cfg(not(feature = "renderer-cell-liquid-policy"))]
            if texture.flags & TEXTURE_LIQUID == 0 {
                match leaf_invariant_facing(plane, u16::from(visible.face.flags), bounds) {
                    Some(false) => continue,
                    Some(true) => {
                        visible.bounds.surface_index |= VISIBLE_INVARIANT_FRONT_BIT;
                    }
                    None => {}
                }
            }
            if write != read {
                unsafe { move_visible_face(faces.add(read), faces.add(write)) };
                #[cfg(feature = "renderer-compact-cell-stream")]
                unsafe {
                    ptr::write(planes.add(write), plane);
                }
            }
            write += 1;
        }
        unsafe { self.visible_faces.set_len(write) };
        #[cfg(feature = "renderer-compact-cell-stream")]
        unsafe {
            self.visible_face_planes.set_len(write);
        }
    }
}

#[optimize(size)]
fn push_centerprint_text(
    packets: &mut Vec<QuadTexturedMaterial>,
    x: i16,
    y: i16,
    text: &str,
    color: (u8, u8, u8),
) {
    let material = TextureMaterial::opaque(clut_texture(), FONT_TPAGE, color);
    for glyph in quake_core::text::TextGlyphs::new(text, x, y, 8, 8) {
        if packets.len() == packets.capacity() {
            break;
        }
        let u = (glyph.character & 0x0f) * 8;
        let v = (glyph.character >> 4) * 8;
        packets.push(QuadTexturedMaterial::with_material(
            [
                (glyph.x, glyph.y),
                (glyph.x + 8, glyph.y),
                (glyph.x, glyph.y + 8),
                (glyph.x + 8, glyph.y + 8),
            ],
            [(u, v), (u + 8, v), (u, v + 8), (u + 8, v + 8)],
            material,
        ));
    }
}

/// Keep the slice metadata materialized across the MIPS call boundary.
///
/// This is deliberately out of line: LLVM's experimental MIPS-I backend has
/// appears to miscompile the equivalent chained `Vec::as_mut_ptr`/`Vec::len`
/// call in the full E1M1 renderer: the registration call observes zero
/// packets even though the following Rust expression observes the populated
/// vector. The fixed ABI boundary is covered by the real-MIPS visual parity
/// gate; this is an observed codegen sensitivity, not a minimized compiler
/// testcase.
#[optimize(size)]
#[inline(never)]
fn register_hud_packets(packets: &mut Vec<QuadTexturedMaterial>) -> usize {
    let packet_start = packets.as_mut_ptr().cast::<u32>();
    let packet_count = packets.len();
    unsafe {
        crate::platform::register_screen_packets(
            packet_start,
            packet_count,
            QuadTexturedMaterial::WORDS,
        )
    }
}

#[optimize(size)]
const fn crosshair_packets() -> [RectFlat; 4] {
    [
        RectFlat::new(159, 118, 2, 1, 0x80, 0x80, 0x80),
        RectFlat::new(159, 121, 2, 1, 0x80, 0x80, 0x80),
        RectFlat::new(158, 119, 1, 2, 0x80, 0x80, 0x80),
        RectFlat::new(161, 119, 1, 2, 0x80, 0x80, 0x80),
    ]
}

#[optimize(size)]
fn push_classic_hud(
    packets: &mut Vec<QuadTexturedMaterial>,
    map: &ResidentMap,
    view: HudView,
) -> bool {
    if !push_inventory_strip(packets, map, &view) {
        return false;
    }
    if !push_status_bar_background(packets, map) {
        return false;
    }

    if view.armor_tier.is_some() {
        let Some(armor) = map.picture(view.armor_picture()) else {
            return false;
        };
        push_picture(packets, 0, 216, armor, (0x80, 0x80, 0x80));
    }
    if !push_big_field(packets, map, 24, 216, view.armor, 25) {
        return false;
    }

    let Some(face) = map.picture(view.face_picture()) else {
        return false;
    };
    push_picture(packets, 112, 216, face, (0x80, 0x80, 0x80));
    if !push_big_field(packets, map, 136, 216, view.health, 25) {
        return false;
    }

    if let Some(ammo_id) = view.ammo_picture() {
        let Some(ammo) = map.picture(ammo_id) else {
            return false;
        };
        push_picture(packets, 224, 216, ammo, (0x80, 0x80, 0x80));
        if !push_big_field(packets, map, 248, 216, view.active_ammo(), 10) {
            return false;
        }
    }

    true
}

/// Reconstruct the original 320x24 `sbar` from exact crops packed into the
/// otherwise fragmented tail of the resident picture band.
#[optimize(size)]
#[inline(never)]
fn push_status_bar_background(packets: &mut Vec<QuadTexturedMaterial>, map: &ResidentMap) -> bool {
    const DESTINATIONS: [(i16, i16); 29] = [
        (0, 216),
        (128, 216),
        (256, 216),
        (0, 224),
        (16, 224),
        (48, 224),
        (80, 224),
        (112, 224),
        (144, 224),
        (176, 224),
        (272, 224),
        (296, 224),
        (0, 232),
        (16, 232),
        (32, 232),
        (56, 232),
        (80, 232),
        (104, 232),
        (232, 232),
        (240, 232),
        (248, 232),
        (256, 232),
        (264, 232),
        (272, 232),
        (280, 232),
        (288, 232),
        (296, 232),
        (304, 232),
        (312, 232),
    ];
    for (index, (x, y)) in DESTINATIONS.into_iter().enumerate() {
        let Some(piece) = map.picture_at(GraphicsPictureId::StatusBar0.index() + index) else {
            return false;
        };
        push_picture_edge_clamped(packets, x, y, piece, (0x80, 0x80, 0x80));
    }
    true
}

/// The compact overlay this port used before exposing the original status-bar
/// depth. It deliberately leaves the world visible between independent
/// health, armor, ammo, key and artifact clusters.
#[optimize(size)]
fn push_minimal_hud(
    packets: &mut Vec<QuadTexturedMaterial>,
    map: &ResidentMap,
    view: HudView,
) -> bool {
    let Some(face) = map.picture(view.face_picture()) else {
        return false;
    };
    if !push_counter(packets, map, 4, 212, face, view.health, 25, false) {
        return false;
    }

    let Some(armor) = map.picture(view.armor_picture()) else {
        return false;
    };
    if !push_counter(packets, map, 4, 184, armor, view.armor, 25, false) {
        return false;
    }

    if let Some(ammo_id) = view.ammo_picture() {
        let Some(ammo) = map.picture(ammo_id) else {
            return false;
        };
        if !push_counter(packets, map, 292, 212, ammo, view.active_ammo(), 10, true) {
            return false;
        }
    }

    let mut key_x = 300i16;
    for (bit, id) in [
        (1u8, GraphicsPictureId::Key1),
        (2u8, GraphicsPictureId::Key2),
    ] {
        if view.keys & bit == 0 {
            continue;
        }
        let Some(key) = map.picture(id) else {
            return false;
        };
        push_picture(packets, key_x, 192, key, (0x80, 0x80, 0x80));
        key_x -= i16::from(key.width);
    }

    for index in 0..4u8 {
        if view.runes & (1 << index) == 0 {
            continue;
        }
        let Some(rune) = map.picture(HudView::rune_picture(index)) else {
            return false;
        };
        push_picture(
            packets,
            284 + i16::from(index) * i16::from(rune.width),
            176,
            rune,
            (0x80, 0x80, 0x80),
        );
    }

    let mut power_y = 164i16;
    for (kind, id) in [
        (
            quake_core::survival::PowerupKind::Ring,
            GraphicsPictureId::PowerInvisibility,
        ),
        (
            quake_core::survival::PowerupKind::Pentagram,
            GraphicsPictureId::PowerInvulnerability,
        ),
        (
            quake_core::survival::PowerupKind::Biosuit,
            GraphicsPictureId::PowerBiosuit,
        ),
        (
            quake_core::survival::PowerupKind::Quad,
            GraphicsPictureId::PowerQuad,
        ),
    ] {
        let seconds = view.powerup_seconds[kind.index()];
        if seconds == 0 {
            continue;
        }
        let Some(power) = map.picture(id) else {
            return false;
        };
        push_picture(packets, 4, power_y, power, (0x80, 0x80, 0x80));
        push_shadowed_u16(
            packets,
            4 + i16::from(power.width) + 2,
            power_y + 4,
            u16::from(seconds),
            (0x80, 0x80, 0x80),
        );
        power_y -= i16::from(power.height);
    }

    true
}

/// `Sbar_DrawInventory`: the original 24-pixel `ibar`, seven weapon slots,
/// four alternate-colour ammo totals, keys, artifacts and sigils.
#[optimize(size)]
#[inline(never)]
fn push_inventory_strip(
    packets: &mut Vec<QuadTexturedMaterial>,
    map: &ResidentMap,
    view: &HudView,
) -> bool {
    let mut x = 0i16;
    let mut index = 0usize;
    while index < 3 {
        let Some(bar) = map.picture_at(GraphicsPictureId::InventoryBar0.index() + index) else {
            return false;
        };
        push_picture(packets, x, 192, bar, (0x80, 0x80, 0x80));
        x += i16::from(bar.width);
        index += 1;
    }

    index = 0;
    while index < 7 {
        if view.owns_weapon_slot(index) {
            let Some(weapon) =
                map.picture_at(GraphicsPictureId::InventoryWeaponShotgun.index() + index)
            else {
                return false;
            };
            push_picture(packets, index as i16 * 24, 200, weapon, (0x80, 0x80, 0x80));
        }
        index += 1;
    }

    index = 0;
    while index < 4 {
        push_inventory_u16(packets, 10 + index as i16 * 48, 192, view.ammo_pools[index]);
        index += 1;
    }

    for (index, (bit, id)) in [
        (1u8, GraphicsPictureId::Key1),
        (2u8, GraphicsPictureId::Key2),
    ]
    .into_iter()
    .enumerate()
    {
        if view.keys & bit == 0 {
            continue;
        }
        let Some(key) = map.picture(id) else {
            return false;
        };
        push_picture(
            packets,
            192 + index as i16 * 16,
            200,
            key,
            (0x80, 0x80, 0x80),
        );
    }

    for (index, (kind, id)) in [
        (
            quake_core::survival::PowerupKind::Ring,
            GraphicsPictureId::PowerInvisibility,
        ),
        (
            quake_core::survival::PowerupKind::Pentagram,
            GraphicsPictureId::PowerInvulnerability,
        ),
        (
            quake_core::survival::PowerupKind::Biosuit,
            GraphicsPictureId::PowerBiosuit,
        ),
        (
            quake_core::survival::PowerupKind::Quad,
            GraphicsPictureId::PowerQuad,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        if view.powerup_seconds[kind.index()] == 0 {
            continue;
        }
        let Some(power) = map.picture(id) else {
            return false;
        };
        push_picture(
            packets,
            224 + index as i16 * 16,
            200,
            power,
            (0x80, 0x80, 0x80),
        );
    }

    for index in 0..4u8 {
        if view.runes & (1 << index) == 0 {
            continue;
        }
        let Some(rune) = map.picture(HudView::rune_picture(index)) else {
            return false;
        };
        push_picture(
            packets,
            288 + i16::from(index) * 8,
            200,
            rune,
            (0x80, 0x80, 0x80),
        );
    }

    true
}

#[optimize(size)]
fn picture_upload_rect(picture: GraphicsPicture) -> Option<psx_vram::VramRect> {
    if picture.width == 0 || picture.height == 0 || picture.width & 1 != 0 {
        return None;
    }
    let page_x = (picture.tpage & 0x000f) * 64;
    let page_y = ((picture.tpage >> 4) & 1) * 256;
    Some(psx_vram::VramRect::new(
        page_x.checked_add(u16::from(picture.u) / 2)?,
        page_y.checked_add(u16::from(picture.v))?,
        u16::from(picture.width) / 2,
        u16::from(picture.height),
    ))
}

#[optimize(size)]
fn push_inventory_u16(packets: &mut Vec<QuadTexturedMaterial>, x: i16, y: i16, value: u16) {
    let value = value.min(999);
    let mut glyphs = [b' '; 3];
    glyphs[2] = 18 + (value % 10) as u8;
    if value >= 10 {
        glyphs[1] = 18 + ((value / 10) % 10) as u8;
    }
    if value >= 100 {
        glyphs[0] = 18 + ((value / 100) % 10) as u8;
    }
    // Every byte is an ASCII space or one of the original alternate digit
    // glyphs 18..=27.
    let text = unsafe { core::str::from_utf8_unchecked(&glyphs) };
    push_text(packets, x, y, text, (0x80, 0x80, 0x80));
}

#[optimize(size)]
fn push_counter(
    packets: &mut Vec<QuadTexturedMaterial>,
    map: &ResidentMap,
    mut x: i16,
    y: i16,
    icon: GraphicsPicture,
    value: u16,
    red_value: u16,
    right_aligned: bool,
) -> bool {
    push_picture(packets, x, y, icon, (0x80, 0x80, 0x80));
    let color = if value <= red_value {
        (0x80, 0, 0)
    } else {
        (0x80, 0x80, 0x80)
    };
    if right_aligned {
        let Some(digit) = map.picture_at(GraphicsPictureId::Number0.index()) else {
            return false;
        };
        x = quake_core::hud::right_aligned_counter_x(x, i16::from(digit.width), 2, value);
    } else {
        x += i16::from(icon.width) + 2;
    }
    push_big_u16(packets, map, x, y, value, color)
}

#[optimize(size)]
fn push_big_field(
    packets: &mut Vec<QuadTexturedMaterial>,
    map: &ResidentMap,
    field_x: i16,
    y: i16,
    value: u16,
    red_value: u16,
) -> bool {
    let color = if value <= red_value {
        (0x80, 0, 0)
    } else {
        (0x80, 0x80, 0x80)
    };
    let x = quake_core::hud::right_aligned_counter_x(field_x + 72, 24, 0, value);
    push_big_u16(packets, map, x, y, value, color)
}

#[optimize(size)]
fn push_big_u16(
    packets: &mut Vec<QuadTexturedMaterial>,
    map: &ResidentMap,
    mut x: i16,
    y: i16,
    value: u16,
    color: (u8, u8, u8),
) -> bool {
    let value = value.min(999);
    let digits = [
        ((value / 100) % 10) as u8,
        ((value / 10) % 10) as u8,
        (value % 10) as u8,
    ];
    let first = if value >= 100 {
        0
    } else if value >= 10 {
        1
    } else {
        2
    };
    for (index, digit) in digits.into_iter().enumerate() {
        if index < first {
            continue;
        }
        let Some(picture) = map.picture_at(GraphicsPictureId::Number0.index() + digit as usize)
        else {
            return false;
        };
        push_picture(packets, x, y, picture, color);
        x += i16::from(picture.width);
    }
    true
}

#[optimize(size)]
fn push_picture(
    packets: &mut Vec<QuadTexturedMaterial>,
    x: i16,
    y: i16,
    picture: GraphicsPicture,
    color: (u8, u8, u8),
) {
    if packets.len() >= packets.capacity() {
        return;
    }
    let right = x.saturating_add(i16::from(picture.width));
    let bottom = y.saturating_add(i16::from(picture.height));
    let u1 = picture.u.wrapping_add(picture.width);
    let v1 = picture.v.wrapping_add(picture.height);
    packets.push(QuadTexturedMaterial::with_material(
        [(x, y), (right, y), (x, bottom), (right, bottom)],
        [
            (picture.u, picture.v),
            (u1, picture.v),
            (picture.u, v1),
            (u1, v1),
        ],
        TextureMaterial::opaque(clut_texture(), picture.tpage, color),
    ));
}

/// Draw a tightly packed opaque crop without letting affine endpoint rounding
/// reach the first texel beyond it. Ordinary HUD sprites retain transparent
/// source margins; the exact `sbar` slices do not, so they need their far UV
/// endpoints pinned to the final valid texel.
#[optimize(size)]
fn push_picture_edge_clamped(
    packets: &mut Vec<QuadTexturedMaterial>,
    x: i16,
    y: i16,
    picture: GraphicsPicture,
    color: (u8, u8, u8),
) {
    if packets.len() >= packets.capacity() {
        return;
    }
    let right = x.saturating_add(i16::from(picture.width));
    let bottom = y.saturating_add(i16::from(picture.height));
    let u1 = picture.u.wrapping_add(picture.width.saturating_sub(1));
    let v1 = picture.v.wrapping_add(picture.height.saturating_sub(1));
    packets.push(QuadTexturedMaterial::with_material(
        [(x, y), (right, y), (x, bottom), (right, bottom)],
        [
            (picture.u, picture.v),
            (u1, picture.v),
            (picture.u, v1),
            (u1, v1),
        ],
        TextureMaterial::opaque(clut_texture(), picture.tpage, color),
    ));
}

#[optimize(size)]
fn push_text_hud(packets: &mut Vec<QuadTexturedMaterial>, view: HudView) {
    let health_color = if view.health <= 25 {
        (0x80, 0x28, 0x28)
    } else {
        (0x80, 0x72, 0x58)
    };
    push_shadowed_text(packets, 8, 220, "HEALTH", health_color);
    push_shadowed_u16(packets, 8, 230, view.health, health_color);

    push_shadowed_text(packets, 112, 220, "ARMOR", (0x48, 0x68, 0x80));
    push_shadowed_u16(packets, 112, 230, view.armor, (0x48, 0x68, 0x80));

    if view.uses_ammo {
        push_shadowed_text(packets, 208, 210, view.weapon_label, (0x80, 0x64, 0x38));
        push_shadowed_text(packets, 208, 220, view.ammo_label, (0x80, 0x64, 0x38));
        push_shadowed_u16(packets, 208, 230, view.active_ammo(), (0x80, 0x64, 0x38));
    } else {
        push_shadowed_text(packets, 208, 220, view.weapon_label, (0x80, 0x64, 0x38));
    }

    if view.keys & 1 != 0 {
        push_shadowed_text(packets, 272, 220, "K1", (0x70, 0x70, 0x38));
    }
    if view.keys & 2 != 0 {
        push_shadowed_text(packets, 296, 220, "K2", (0x70, 0x70, 0x38));
    }

    let mut column = 8i16;
    for kind in quake_core::survival::PowerupKind::ALL {
        let seconds = view.powerup_seconds[kind.index()];
        if seconds == 0 {
            continue;
        }
        let color = if seconds <= POWERUP_WARNING_SECONDS {
            (0x80, 0x28, 0x28)
        } else {
            (0x80, 0x50, 0x80)
        };
        push_shadowed_text(packets, column, 206, kind.label(), color);
        push_shadowed_u16(packets, column + 40, 206, u16::from(seconds), color);
        column += 72;
    }
}

#[cfg(feature = "visual-parity-regression")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct ScopedWindowAudit {
    windowed: u32,
    restored: u32,
    failures: u32,
}

#[cfg(feature = "visual-parity-regression")]
#[optimize(size)]
unsafe fn audit_scoped_window_packets(start: *mut u32, end: *mut u32) -> ScopedWindowAudit {
    let mut audit = ScopedWindowAudit::default();
    let mut batch_selectors = 0u32;
    let mut batch_resets = 0u32;
    let mut packet = start;
    while packet < end {
        let remaining = unsafe { end.offset_from(packet) };
        if remaining <= 0 {
            audit.failures = audit.failures.wrapping_add(1);
            break;
        }
        let data_words = (unsafe { packet.read() } >> 24) as usize;
        if data_words + 1 > remaining as usize {
            audit.failures = audit.failures.wrapping_add(1);
            break;
        }
        if data_words != 0 {
            let selector = unsafe { packet.add(1).read() };
            if selector & 0xff00_0000 == 0xe200_0000 {
                if data_words == 1 {
                    if selector == TextureWindow::NONE.word() {
                        batch_resets = batch_resets.wrapping_add(1);
                    } else {
                        batch_selectors = batch_selectors.wrapping_add(1);
                    }
                } else if selector != TextureWindow::NONE.word() {
                    audit.windowed = audit.windowed.wrapping_add(1);
                    if unsafe { packet.add(data_words).read() } == TextureWindow::NONE.word() {
                        audit.restored = audit.restored.wrapping_add(1);
                    } else {
                        audit.failures = audit.failures.wrapping_add(1);
                    }
                } else {
                    audit.restored = audit.restored.wrapping_add(1);
                }
            }
        }
        packet = unsafe { packet.add(data_words + 1) };
    }
    if batch_selectors != 0 {
        audit.windowed = audit.windowed.wrapping_add(batch_selectors);
        // Two sequential sky-layer selectors share one terminal reset.
        if batch_selectors == batch_resets.wrapping_add(1) {
            audit.restored = audit.restored.wrapping_add(batch_selectors);
        } else {
            audit.failures = audit.failures.wrapping_add(1);
        }
    } else if batch_resets != 0 {
        audit.failures = audit.failures.wrapping_add(1);
    }
    audit
}

/// Top and pitch of the Options rows. A music-capable disc has fourteen rows,
/// so that version uses a dense console rhythm; a silent disc has eleven and
/// keeps its Back row above the 240-line display edge.
#[optimize(size)]
fn options_row_top(_view: MenuView) -> i16 {
    60
}

#[optimize(size)]
fn options_row_pitch(view: MenuView) -> i16 {
    if view.music_available {
        12
    } else {
        15
    }
}

/// `M_DrawSlider`: left cap (128), ten rail glyphs (129), right cap (130),
/// then the slider thumb (131) over the rail. These live in conchars but are
/// outside UTF-8 ASCII, so they deliberately bypass `push_text`.
#[optimize(size)]
fn push_menu_slider(packets: &mut Vec<QuadTexturedMaterial>, x: i16, y: i16, value: u8) {
    push_menu_glyph(packets, x - 8, y, 128);
    for index in 0..10 {
        push_menu_glyph(packets, x + index * 8, y, 129);
    }
    push_menu_glyph(packets, x + 80, y, 130);
    let thumb = i16::from(value.min(quake_core::menu::VOLUME_STEPS)) * 72
        / i16::from(quake_core::menu::VOLUME_STEPS);
    push_menu_glyph(packets, x + thumb, y, 131);
}

#[optimize(size)]
fn push_menu_glyph(packets: &mut Vec<QuadTexturedMaterial>, x: i16, y: i16, character: u8) {
    if packets.len() == packets.capacity() {
        return;
    }
    let u = (character & 0x0f) * 8;
    let v = (character >> 4) * 8;
    packets.push(QuadTexturedMaterial::with_material(
        [(x, y), (x + 8, y), (x, y + 8), (x + 8, y + 8)],
        [(u, v), (u + 8, v), (u, v + 8), (u + 8, v + 8)],
        TextureMaterial::opaque(clut_texture(), FONT_TPAGE, (0x80, 0x80, 0x80)),
    ));
}

#[optimize(size)]
fn menu_row_color(index: u8, selected: u8) -> (u8, u8, u8) {
    if index == selected {
        (0x80, 0x80, 0x80)
    } else {
        (0x80, 0x62, 0x38)
    }
}

#[optimize(size)]
fn push_centered_text(
    packets: &mut Vec<QuadTexturedMaterial>,
    y: i16,
    text: &str,
    color: (u8, u8, u8),
) {
    let width = text.len().min(40) as i16 * 8;
    push_shadowed_text(packets, (320 - width) / 2, y, text, color);
}

#[optimize(size)]
fn push_shadowed_text(
    packets: &mut Vec<QuadTexturedMaterial>,
    x: i16,
    y: i16,
    text: &str,
    color: (u8, u8, u8),
) {
    push_text(packets, x + 1, y + 1, text, (0x18, 0x18, 0x18));
    push_text(packets, x, y, text, color);
}

#[optimize(size)]
fn push_shadowed_u16(
    packets: &mut Vec<QuadTexturedMaterial>,
    x: i16,
    y: i16,
    value: u16,
    color: (u8, u8, u8),
) {
    let mut digits = [b'0'; 5];
    let mut cursor = digits.len();
    let mut remaining = value;
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    // Every written byte is in b'0'..=b'9'. Avoid carrying a UTF-8 error
    // path into the fixed PS1 hot path.
    let text = unsafe { core::str::from_utf8_unchecked(&digits[cursor..]) };
    push_shadowed_text(packets, x, y, text, color);
}

/// `Sbar_IntermissionOverlay` prints the completion time as `M:SS`, with the
/// seconds always padded to two digits.
#[optimize(size)]
fn push_shadowed_clock(
    packets: &mut Vec<QuadTexturedMaterial>,
    x: i16,
    y: i16,
    minutes: u16,
    seconds: u16,
    color: (u8, u8, u8),
) {
    let mut text = [b'0'; 6];
    let mut cursor = 3usize;
    let mut remaining = minutes.min(999);
    loop {
        cursor -= 1;
        text[cursor] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 || cursor == 0 {
            break;
        }
    }
    let seconds = (seconds % 60) as u8;
    text[3] = b':';
    text[4] = b'0' + seconds / 10;
    text[5] = b'0' + seconds % 10;
    // Every byte written is a digit or a colon, so the slice is ASCII by
    // construction; the PS1 hot path carries no UTF-8 error branch.
    let text = unsafe { core::str::from_utf8_unchecked(&text[cursor..]) };
    push_shadowed_text(packets, x, y, text, color);
}

#[optimize(size)]
fn push_text(
    packets: &mut Vec<QuadTexturedMaterial>,
    mut x: i16,
    y: i16,
    text: &str,
    color: (u8, u8, u8),
) {
    let material = TextureMaterial::opaque(clut_texture(), FONT_TPAGE, color);
    for character in text.bytes().take(40) {
        if character != b' ' && packets.len() < packets.capacity() {
            let u = (character & 0x0f) * 8;
            let v = (character >> 4) * 8;
            packets.push(QuadTexturedMaterial::with_material(
                [(x, y), (x + 8, y), (x, y + 8), (x + 8, y + 8)],
                [(u, v), (u + 8, v), (u, v + 8), (u + 8, v + 8)],
                material,
            ));
        }
        x += 8;
    }
}

fn quake_frustum(camera: Camera) -> [AabbClipPlane; 4] {
    let yaw = camera.angles[1] as u16 & 0x0fff;
    let pitch = camera.angles[0] as u16 & 0x0fff;
    let roll = camera.angles[2] as u16 & 0x0fff;
    let sy = sin_q12(yaw);
    let cy = cos_q12(yaw);
    let sp = sin_q12(pitch);
    let cp = cos_q12(pitch);
    let sr = sin_q12(roll);
    let cr = cos_q12(roll);
    let multiply = |left: i32, right: i32| mul_q12_i32(left, right);
    let clamp = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

    let forward = [clamp(multiply(cp, cy)), clamp(multiply(cp, sy)), clamp(-sp)];
    let right = [
        clamp(multiply(multiply(-sr, sp), cy) + multiply(-cr, -sy)),
        clamp(multiply(multiply(-sr, sp), sy) + multiply(-cr, cy)),
        clamp(multiply(-sr, cp)),
    ];
    let up = [
        clamp(multiply(multiply(cr, sp), cy) + multiply(-sr, -sy)),
        clamp(multiply(multiply(cr, sp), sy) + multiply(-sr, cy)),
        clamp(multiply(cr, cp)),
    ];
    let normals = [
        add_normal(forward, right),
        subtract_normal(forward, right),
        add_normal(forward, up),
        subtract_normal(forward, up),
    ];
    normals.map(|normal| {
        let distance = mul_q12_i32(camera.origin.x, normal[0] as i32)
            .saturating_add(mul_q12_i32(camera.origin.y, normal[1] as i32))
            .saturating_add(mul_q12_i32(camera.origin.z, normal[2] as i32));
        let signbits = u8::from(normal[0] < 0)
            | (u8::from(normal[1] < 0) << 1)
            | (u8::from(normal[2] < 0) << 2);
        AabbClipPlane {
            normal,
            kind: 3,
            signbits,
            distance,
        }
    })
}

fn add_normal(left: [i16; 3], right: [i16; 3]) -> [i16; 3] {
    [
        left[0].saturating_add(right[0]),
        left[1].saturating_add(right[1]),
        left[2].saturating_add(right[2]),
    ]
}

fn subtract_normal(left: [i16; 3], right: [i16; 3]) -> [i16; 3] {
    [
        left[0].saturating_sub(right[0]),
        left[1].saturating_sub(right[1]),
        left[2].saturating_sub(right[2]),
    ]
}

fn animate_special_surface(vertices: &mut [ClassicAffineVertex], texture: TextureInfo, frame: u32) {
    // Liquid pixels are already warped densely in their atlas tile. Applying
    // another corner-only UV wobble here would reintroduce the large affine
    // streak which the original per-texel algorithm removes.
    if texture.flags & TEXTURE_SKY != 0 {
        let scroll =
            frame.wrapping_mul(LEGACY_SKY_SCROLL_TEXELS_PER_SECOND) / ANIMATION_FRAMES_PER_SECOND;
        for vertex in vertices {
            vertex.uv[0] = vertex.uv[0].wrapping_add(scroll as u8);
        }
    }
}

/// One staged GP0(E2) command for a complete sky layer.
#[repr(C, align(4))]
struct SkyWindowPacket {
    tag: u32,
    command: u32,
}

impl SkyWindowPacket {
    const fn new(command: u32) -> Self {
        Self {
            tag: (1 << 24) | SKY_OT_SLOT,
            command,
        }
    }
}

const _: () = assert!(
    core::mem::size_of::<SkyWindowPacket>()
        == SKY_WINDOW_PACKET_WORDS * core::mem::size_of::<u32>()
);
const _: () =
    assert!(core::mem::size_of::<QuadTextured>() == SKY_QUAD_WORDS * core::mem::size_of::<u32>());

/// Draw Quake's two sky layers as a bounded view-ray background.
///
/// Visible sky brushes select the material but emit no geometry. The lattice
/// is submitted behind the world, so those brushes remain apertures through
/// which the background is visible. Unlike per-brush adaptive subdivision,
/// packet and CPU cost are constant even when the camera touches a sky face.
unsafe fn submit_view_ray_sky_background(
    texture: TextureInfo,
    view: QuakeViewTransform,
    frame: u32,
    output: *mut u32,
) -> ClassicAffineSubmit {
    let width = texture.size.x.clamp(8, 128) as u8;
    let height = texture.size.y.clamp(8, 128) as u8;
    debug_assert!(width.is_power_of_two());
    debug_assert!(height.is_power_of_two());
    debug_assert!(texture.atlas.x.is_multiple_of(width));
    debug_assert!(texture.atlas.y.is_multiple_of(height));
    debug_assert!(texture.atlas.x as u16 + width as u16 * 2 <= 256);
    debug_assert!(texture.atlas.y as u16 + height as u16 <= 256);
    let foreground_window =
        TextureWindow::power_of_two_tile(texture.atlas.x, texture.atlas.y, width, height);
    let background_window = TextureWindow::power_of_two_tile(
        texture.atlas.x.wrapping_add(width),
        texture.atlas.y,
        width,
        height,
    );
    let scroll = |cycle_seconds: u32| {
        ((u64::from(frame) * u64::from(width)
            / u64::from(ANIMATION_FRAMES_PER_SECOND * cycle_seconds))
            & 0xff) as u8
    };
    let foreground_scroll = [
        scroll(SKY_FOREGROUND_CYCLE_SECONDS),
        scroll(SKY_FOREGROUND_CYCLE_SECONDS),
    ];
    let background_scroll = [
        scroll(SKY_BACKGROUND_CYCLE_SECONDS),
        scroll(SKY_BACKGROUND_CYCLE_SECONDS),
    ];
    let foreground_material =
        TextureMaterial::opaque(clut_texture(), texture.texture_page, (0x80, 0x80, 0x80));
    let background_material =
        TextureMaterial::opaque(clut_texture(), texture.texture_page, (0x80, 0x80, 0x80));

    let mut samples = [[[0i32; 2]; SKY_FOREGROUND_COLUMNS + 1]; SKY_FOREGROUND_ROWS + 1];
    for (row, sample_row) in samples.iter_mut().enumerate() {
        let y = (row * SCREEN_HEIGHT as usize / SKY_FOREGROUND_ROWS) as i16;
        for (column, sample) in sample_row.iter_mut().enumerate() {
            let x = (column * SCREEN_WIDTH as usize / SKY_FOREGROUND_COLUMNS) as i16;
            let ray = quake_core::sky::screen_view_ray(
                [x, y],
                [SCREEN_WIDTH / 2, SCREEN_HEIGHT / 2],
                160,
                view.rotation.m,
            );
            *sample = quake_core::sky::directional_texel(ray, width);
        }
    }

    let mut next = output;
    // The tagged stream is linked by prepending packets. Stage the reset
    // first so it executes after both layer passes and before world geometry.
    unsafe {
        next.cast::<SkyWindowPacket>()
            .write(SkyWindowPacket::new(TextureWindow::NONE.word()));
        next = next.add(SKY_WINDOW_PACKET_WORDS);
    }
    // Every packet targets one prepend-only OT slot. Emit the foreground
    // first so all subsequently emitted background cells execute behind it.
    for row in 0..SKY_FOREGROUND_ROWS {
        let y0 = (row * SCREEN_HEIGHT as usize / SKY_FOREGROUND_ROWS) as i16;
        let y1 = ((row + 1) * SCREEN_HEIGHT as usize / SKY_FOREGROUND_ROWS) as i16;
        for column in 0..SKY_FOREGROUND_COLUMNS {
            let x0 = (column * SCREEN_WIDTH as usize / SKY_FOREGROUND_COLUMNS) as i16;
            let x1 = ((column + 1) * SCREEN_WIDTH as usize / SKY_FOREGROUND_COLUMNS) as i16;
            let vertices = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
            let cell_samples = [
                samples[row][column],
                samples[row][column + 1],
                samples[row + 1][column],
                samples[row + 1][column + 1],
            ];

            let foreground_uv = quake_core::sky::packet_quad_uv(
                cell_samples,
                [texture.atlas.x, texture.atlas.y],
                [width, height],
                foreground_scroll,
            )
            .map(|[u, v]| (u, v));
            unsafe {
                let mut quad =
                    QuadTextured::with_material(vertices, foreground_uv, foreground_material);
                quad.tag = ((QuadTextured::WORDS as u32) << 24) | SKY_OT_SLOT;
                next.cast::<QuadTextured>().write(quad);
                next = next.add(SKY_QUAD_WORDS);
            }
        }
    }
    unsafe {
        next.cast::<SkyWindowPacket>()
            .write(SkyWindowPacket::new(foreground_window.word()));
        next = next.add(SKY_WINDOW_PACKET_WORDS);
    }

    const COLUMN_STEP: usize = SKY_FOREGROUND_COLUMNS / SKY_BACKGROUND_COLUMNS;
    const ROW_STEP: usize = SKY_FOREGROUND_ROWS / SKY_BACKGROUND_ROWS;
    for row in 0..SKY_BACKGROUND_ROWS {
        let y0 = (row * SCREEN_HEIGHT as usize / SKY_BACKGROUND_ROWS) as i16;
        let y1 = ((row + 1) * SCREEN_HEIGHT as usize / SKY_BACKGROUND_ROWS) as i16;
        let sample_row = row * ROW_STEP;
        for column in 0..SKY_BACKGROUND_COLUMNS {
            let x0 = (column * SCREEN_WIDTH as usize / SKY_BACKGROUND_COLUMNS) as i16;
            let x1 = ((column + 1) * SCREEN_WIDTH as usize / SKY_BACKGROUND_COLUMNS) as i16;
            let vertices = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
            let sample_column = column * COLUMN_STEP;
            let cell_samples = [
                samples[sample_row][sample_column],
                samples[sample_row][sample_column + COLUMN_STEP],
                samples[sample_row + ROW_STEP][sample_column],
                samples[sample_row + ROW_STEP][sample_column + COLUMN_STEP],
            ];
            let background_uv = quake_core::sky::packet_quad_uv(
                cell_samples,
                [texture.atlas.x.wrapping_add(width), texture.atlas.y],
                [width, height],
                background_scroll,
            )
            .map(|[u, v]| (u, v));
            unsafe {
                let mut quad =
                    QuadTextured::with_material(vertices, background_uv, background_material);
                quad.tag = ((QuadTextured::WORDS as u32) << 24) | SKY_OT_SLOT;
                next.cast::<QuadTextured>().write(quad);
                next = next.add(SKY_QUAD_WORDS);
            }
        }
    }
    unsafe {
        next.cast::<SkyWindowPacket>()
            .write(SkyWindowPacket::new(background_window.word()));
        next = next.add(SKY_WINDOW_PACKET_WORDS);
    }

    ClassicAffineSubmit {
        next_packet: next,
        packets: (SKY_FOREGROUND_CELLS + SKY_BACKGROUND_CELLS + SKY_WINDOW_PACKET_COUNT) as u32,
        hardware_triangles: ((SKY_FOREGROUND_CELLS + SKY_BACKGROUND_CELLS) * 2) as u32,
    }
}

fn special_texture_window(texture: TextureInfo) -> TextureWindow {
    let width = (texture.size.x.max(4) as u16 * 2).min(128) as u8;
    let mask_x = texture_window_mask(width);
    let offset_x = texture.atlas.x / 8;
    if texture.flags & TEXTURE_LIQUID != 0 {
        let height = (texture.size.y.max(8) as u16).min(128) as u8;
        TextureWindow::new(
            mask_x,
            texture_window_mask(height),
            offset_x,
            texture.atlas.y / 8,
        )
    } else {
        // The legacy atlas may place sky rows at a non-window-aligned Y.
        // Only U scrolls, so leave V unmasked and preserve its exact address.
        TextureWindow::new(mask_x, 0, offset_x, 0)
    }
}

fn texture_window_mask(size: u8) -> u8 {
    (((!(size - 1)) as u16 & 0x00ff) as u8) / 8
}

#[inline]
fn packet_capacity(next: *mut u32, end: *mut u32, needed_words: usize) -> bool {
    // `ptr.add(needed_words)` would itself be undefined if the speculative
    // result crossed the arena. Both pointers are known members of one arena,
    // so compare the in-bounds distance as an integer instead.
    let remaining = unsafe { end.offset_from(next) };
    remaining >= 0 && needed_words <= remaining as usize
}

/// Capture the staged ordering-table slot carried by every world packet.
/// The final OT linker overwrites those low bits with a DMA address, while
/// retaining the packet word count in the tag's high byte.
#[cfg(feature = "renderer-static-world-reuse")]
unsafe fn capture_static_world_tags(start: *mut u32, end: *mut u32, slots: &mut Vec<u16>) -> bool {
    slots.clear();
    let mut packet = start;
    while packet < end && slots.len() < MAX_STATIC_WORLD_PACKET_SLOTS {
        let tag = unsafe { ptr::read(packet) };
        slots.push(tag as u16);
        let packet_words = ((tag >> 24) as usize).wrapping_add(1);
        if packet_words <= 1 {
            slots.clear();
            return false;
        }
        packet = unsafe { packet.add(packet_words) };
    }
    if packet != end {
        slots.clear();
        return false;
    }
    true
}

/// Restore staged OT slots in an otherwise immutable same-camera packet
/// stream. Packet payloads, projected coordinates, depth order, and stream
/// layout remain those produced by the authoritative writer in this arena's
/// prior frame.
#[cfg(feature = "renderer-static-world-reuse")]
unsafe fn restore_static_world_tags(start: *mut u32, end: *mut u32, slots: &[u16]) -> bool {
    let mut packet = start;
    let mut slot_index = 0usize;
    while packet < end && slot_index < slots.len() {
        let linked_tag = unsafe { ptr::read(packet) };
        let packet_words = ((linked_tag >> 24) as usize).wrapping_add(1);
        if packet_words <= 1 {
            return false;
        }
        unsafe {
            ptr::write(
                packet,
                (linked_tag & 0xff00_0000) | u32::from(*slots.get_unchecked(slot_index)),
            );
        }
        packet = unsafe { packet.add(packet_words) };
        slot_index += 1;
    }
    packet == end && slot_index == slots.len()
}

unsafe fn flush_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    output: *mut u32,
) -> ClassicAffineSubmit {
    if vertex_count == 0 || surface_count == 0 {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }
    #[cfg(not(feature = "renderer-quake-specialized-kernel"))]
    unsafe {
        submit_classic_affine_batch(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            ClassicAffineProfile::QUAKE_REFERENCE,
        )
    }
    #[cfg(feature = "renderer-quake-specialized-kernel")]
    unsafe {
        submit_quake_classic_affine_batch(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
        )
    }
}

/// Submit an ordinary retained-world batch through the authoritative
/// project-then-topology path.
unsafe fn flush_world_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    output: *mut u32,
) -> ClassicAffineSubmit {
    if vertex_count == 0 || surface_count == 0 {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }
    unsafe { flush_batch(vertices, vertex_count, surfaces, surface_count, output) }
}

/// Record the exact ordinary-world stream after the shipping submitter has
/// projected its source vertices. The diagnostic topology pass reuses those
/// projections and does not emit packets or perturb the ordering table.
#[cfg(feature = "renderer-census")]
#[inline(never)]
unsafe fn census_world_batch(
    vertices: *const ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    source_surfaces: *const u16,
    output: *mut u32,
    submitted: ClassicAffineSubmit,
    map_generation: u32,
    frame: u32,
    slab_cache_models: &mut [SubdivisionSlabCacheModel; SUBDIVISION_CACHE_BUDGETS_KIB.len()],
    census: &mut RendererCensus,
) {
    unsafe {
        census_classic_affine_projected_batch_topology(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            ClassicAffineProfile::QUAKE_REFERENCE,
            &mut census.topology,
        );
    }
    let mut requests = [ClassicAffineSubdivisionRequest::default(); SUBDIVISION_REQUEST_CAPACITY];
    let request_count = unsafe {
        collect_classic_affine_projected_subdivision_requests(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            ClassicAffineProfile::QUAKE_REFERENCE,
            requests.as_mut_ptr(),
            requests.len(),
        )
    };
    debug_assert!(request_count <= requests.len());
    for request in &requests[..request_count.min(requests.len())] {
        let source_face = unsafe { ptr::read(source_surfaces.add(request.batch_surface as usize)) };
        // Near-plane clipping creates camera-dependent vertices, so it must
        // keep using the dynamic submitter even when its lattice happens to
        // match a previous frame.
        if source_face == u16::MAX {
            continue;
        }
        for (index, model) in slab_cache_models.iter_mut().enumerate() {
            debug_assert_eq!(model.map_generation, map_generation);
            model.request(
                frame,
                source_face,
                *request,
                &mut census.subdivision_slab_caches[index],
            );
        }
    }
    let words = unsafe { submitted.next_packet.offset_from(output) as u32 };
    census.ordinary_output_packet_bytes = census
        .ordinary_output_packet_bytes
        .wrapping_add(words.wrapping_mul(4));
    census.ordinary_output_packets = census
        .ordinary_output_packets
        .wrapping_add(submitted.packets);
    census.ordinary_output_hardware_triangles = census
        .ordinary_output_hardware_triangles
        .wrapping_add(submitted.hardware_triangles);
}

/// The world and brush-entity batch stages borrow the PS1's 1 KiB data
/// scratchpad (51 x 20-byte vertices, 1,020 of its 1,024 bytes) instead of a
/// DRAM stack array.
/// The projection and packet passes re-read every staged vertex, and
/// scratchpad accesses are single-cycle where DRAM pays wait states. The two
/// users run strictly sequentially, nothing else in the runtime or SDK
/// touches the shared scratchpad reservation, and no DMA reads these vertices.
const _: () = assert!(core::mem::size_of::<BatchVertexStorage>() <= psx_engine::scratchpad::SIZE);

#[inline]
fn scratchpad_batch_vertices() -> &'static mut BatchVertexStorage {
    unsafe { &mut *psx_engine::scratchpad::ptr_at::<BatchVertexStorage>(0) }
}

#[inline]
fn uninit_batch_surfaces() -> BatchSurfaceStorage {
    [const { MaybeUninit::uninit() }; BATCH_MAX_SURFACES]
}

#[cfg(feature = "renderer-fused-materialize-project")]
#[inline]
fn uninit_batch_indexed_sources() -> BatchIndexedSourceStorage {
    [const { MaybeUninit::uninit() }; BATCH_MAX_SURFACES]
}

#[cfg(feature = "renderer-fused-materialize-project")]
#[inline]
fn uninit_batch_visible_indices() -> BatchVisibleIndexStorage {
    [const { MaybeUninit::uninit() }; BATCH_MAX_SURFACES]
}

#[cfg(any(feature = "renderer-census", feature = "renderer-subdivision-cache"))]
#[inline]
fn uninit_batch_source_surfaces() -> BatchSourceSurfaceStorage {
    [const { MaybeUninit::uninit() }; BATCH_MAX_SURFACES]
}

#[cfg(feature = "renderer-topology-cache")]
#[inline]
fn uninit_resident_batch_surfaces() -> ResidentBatchSurfaceStorage {
    [const { MaybeUninit::uninit() }; BATCH_MAX_SURFACES]
}

#[cfg(feature = "renderer-topology-cache")]
#[inline(always)]
fn record_topology_cache_submit(stats: &mut RenderStats, submitted: ClassicAffinePlannedSubmit) {
    stats.topology_cache_hits = stats
        .topology_cache_hits
        .wrapping_add(u32::from(submitted.topology_hit));
    stats.topology_cache_misses = stats
        .topology_cache_misses
        .wrapping_add(u32::from(!submitted.topology_hit));
    stats.topology_invariant_hit_slots = stats
        .topology_invariant_hit_slots
        .wrapping_add(submitted.invariant_hit_slots);
    stats.topology_invariant_miss_slots = stats
        .topology_invariant_miss_slots
        .wrapping_add(submitted.invariant_miss_slots);
}

/// View only the prefix/range that the caller immediately initializes.
///
/// The subdivision submitter owns the final twelve elements as write-only
/// midpoint scratch. No uninitialized element is read: source vertices are
/// materialized before submission and surface descriptors are written before
/// their count is advanced.
#[inline]
unsafe fn batch_vertices_mut(
    storage: &mut BatchVertexStorage,
    first: usize,
    count: usize,
) -> &mut [ClassicAffineVertex] {
    debug_assert!(first + count <= BATCH_MAX_VERTICES);
    unsafe { core::slice::from_raw_parts_mut(storage.as_mut_ptr().add(first).cast(), count) }
}

fn front_facing(map: &ResidentMap, face: Face, point: Vec3I32) -> bool {
    let plane = unsafe { *map.collision_planes().get_unchecked(face.plane as usize) };
    front_facing_plane(plane, face.flags, point)
}

/// Copy one list entry between two slots of the same list, as eleven word
/// loads and stores. The struct-sized `ptr::read`/`ptr::write` pair became an
/// out-of-line `memmove` call per entry, and so did a plain word loop (loop
/// idiom recognition); volatile accesses keep the words as `lw`/`sw`.
///
/// # Safety
/// Both pointers must be valid, aligned `VisibleFace` slots.
#[inline(always)]
unsafe fn move_visible_face(source: *const VisibleFace, destination: *mut VisibleFace) {
    const WORDS: usize = core::mem::size_of::<VisibleFace>() / core::mem::size_of::<u32>();
    let source = source.cast::<u32>();
    let destination = destination.cast::<u32>();
    let mut index = 0;
    while index < WORDS {
        let word = unsafe { ptr::read_volatile(source.add(index)) };
        unsafe { ptr::write_volatile(destination.add(index), word) };
        index += 1;
    }
}

fn face_bounds(map: &ResidentMap, face: Face) -> ([i16; 3], [i16; 3]) {
    let indexed = map.indexed_vertices().expect("validated PSB4 vertices");
    let first = face.first_vertex as usize;
    let count = face.vertex_count as usize;
    debug_assert!(count >= 3);
    // Validated at load: the face's corner range and every corner's position
    // index are in bounds. Widening to i32 keeps the min/max chain free of
    // the sign-extension shuffles a 16-bit accumulator costs on MIPS.
    let corners = &indexed.corners[first..first + count];
    let positions = indexed.positions.as_ptr();
    let mut mins = [i32::from(i16::MAX); 3];
    let mut maxs = [i32::from(i16::MIN); 3];
    for corner in corners {
        let vertex = unsafe { ptr::read(positions.add(corner.position_index as usize)) };
        for axis in 0..3 {
            let value = i32::from(vertex.position[axis]);
            mins[axis] = mins[axis].min(value);
            maxs[axis] = maxs[axis].max(value);
        }
    }
    (
        [mins[0] as i16, mins[1] as i16, mins[2] as i16],
        [maxs[0] as i16, maxs[1] as i16, maxs[2] as i16],
    )
}

/// A point guaranteed to lie inside a convex cooked face. Any convex
/// combination of three authored corners remains in the polygon, unlike its
/// AABB midpoint. `1365 / 4096` is one third to sub-texel precision and keeps
/// the MIPS path free of division helpers.
#[optimize(size)]
#[inline(never)]
fn water_face_sample(map: &ResidentMap, face: CookedDrawSurface) -> Vec3I32 {
    let indexed = map.indexed_vertices().expect("validated PSB4 vertices");
    let first = face.first_corner as usize;
    let corners = &indexed.corners[first..first + 3];
    let positions = indexed.positions.as_ptr();
    let mut sum = [0i32; 3];
    for corner in corners {
        let vertex = unsafe { ptr::read(positions.add(corner.position_index as usize)) };
        for axis in 0..3 {
            sum[axis] += i32::from(vertex.position[axis]);
        }
    }
    Vec3I32 {
        x: sum[0] * 1365,
        y: sum[1] * 1365,
        z: sum[2] * 1365,
    }
}

/// The per-frame face selection: every PVS face in ascending order, kept when
/// its texture draws, it faces the camera and its bounds touch the frustum.
///
/// Out of line on purpose. Inlined into `draw_frame` this loop's invariants
/// (camera origin, texture table, output cursor) were spilled and reloaded
/// from the stack for every face; as its own function they stay in registers.
/// `output` must have capacity for `visible_faces.len()` entries, which the
/// caller guarantees by sizing both vectors to `MAX_VISIBLE_FACE_COUNT`.
#[cfg(feature = "renderer-census")]
#[inline(always)]
fn finish_plane_run(census: &mut SelectionCensus, calls: u32) {
    if calls == 0 {
        return;
    }
    census.plane_run_tests = census.plane_run_tests.wrapping_add(1);
    census.plane_tests_saved = census
        .plane_tests_saved
        .wrapping_add(calls.saturating_sub(1));
    census.max_plane_run = census.max_plane_run.max(calls);
}

/// Exact selection with extra accounting. It lives beside, rather than
/// inside, the shipping loop so enabling the census cannot perturb the code
/// layout or register allocation of a benchmark build.
#[cfg(feature = "renderer-census")]
#[inline(never)]
fn select_frame_faces_census(
    visible_faces: &[VisibleFace],
    active_textures: &[TextureInfo],
    origin: Vec3I32,
    frustum: &[AabbClipPlane; 4],
    water_plane: i16,
    output: &mut Vec<u16>,
) -> SelectionCensus {
    output.clear();
    debug_assert!(output.capacity() >= visible_faces.len());
    let out = output.as_mut_ptr();
    let mut count = 0usize;
    let mut census = SelectionCensus {
        pvs_faces: visible_faces.len() as u32,
        ..SelectionCensus::default()
    };
    let mut plane_key = None;
    let mut plane_run_calls = 0u32;

    for (visible_index, visible) in visible_faces.iter().enumerate() {
        let key = (
            visible.face.plane,
            u16::from(visible.face.flags) & FACE_BACKSIDE,
        );
        if plane_key != Some(key) {
            finish_plane_run(&mut census, plane_run_calls);
            plane_key = Some(key);
            plane_run_calls = 0;
        }

        let texture = unsafe { active_textures.get_unchecked(visible.face.material as usize) };
        if texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) != 0 {
            census.policy_rejects = census.policy_rejects.wrapping_add(1);
            continue;
        }

        let water_blend =
            texture.flags & TEXTURE_LIQUID != 0 && visible.face.plane as i16 == water_plane;
        if !water_blend {
            census.plane_tests = census.plane_tests.wrapping_add(1);
            plane_run_calls = plane_run_calls.wrapping_add(1);
            if !front_facing_compact_plane(visible.plane, u16::from(visible.face.flags), origin) {
                census.backface_rejects = census.backface_rejects.wrapping_add(1);
                continue;
            }
        }

        if scene::aabb_outside_clip4(visible.bounds.mins, visible.bounds.maxs, frustum, 0x0f) {
            census.frustum_rejects = census.frustum_rejects.wrapping_add(1);
            continue;
        }

        let entry = visible_index as u16 | if water_blend { WATER_BLEND_FACE_BIT } else { 0 };
        unsafe { ptr::write(out.add(count), entry) };
        count += 1;
        census.selected_faces = census.selected_faces.wrapping_add(1);
        census.water_blend_faces = census
            .water_blend_faces
            .wrapping_add(u32::from(water_blend));
    }
    finish_plane_run(&mut census, plane_run_calls);
    unsafe { output.set_len(count) };
    debug_assert_eq!(
        census.pvs_faces,
        census
            .policy_rejects
            .wrapping_add(census.backface_rejects)
            .wrapping_add(census.frustum_rejects)
            .wrapping_add(census.selected_faces)
    );
    census
}

#[cfg(feature = "renderer-census")]
#[inline(always)]
fn face_reaches_aabb(
    visible: &VisibleFace,
    active_textures: &[TextureInfo],
    origin: Vec3I32,
    water_plane: i16,
) -> bool {
    let texture = unsafe { active_textures.get_unchecked(visible.face.material as usize) };
    if texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) != 0 {
        return false;
    }
    let water_blend =
        texture.flags & TEXTURE_LIQUID != 0 && visible.face.plane as i16 == water_plane;
    water_blend || front_facing_compact_plane(visible.plane, u16::from(visible.face.flags), origin)
}

/// Measure a conservative union-AABB prepass for one fixed consecutive group
/// size. A rejected union can safely skip all members; `aabb_tests_saved`
/// counts only members the current texture/backface policy would have sent to
/// the individual GTE AABB test.
#[cfg(feature = "renderer-census")]
#[inline(never)]
fn census_face_blocks_for(
    visible_faces: &[VisibleFace],
    active_textures: &[TextureInfo],
    origin: Vec3I32,
    frustum: &[AabbClipPlane; 4],
    water_plane: i16,
    block_size: usize,
) -> BlockCensus {
    let mut census = BlockCensus::default();
    for block in visible_faces.chunks(block_size) {
        census.groups = census.groups.wrapping_add(1);
        let mut mins = block[0].bounds.mins;
        let mut maxs = block[0].bounds.maxs;
        for visible in &block[1..] {
            for axis in 0..3 {
                mins[axis] = mins[axis].min(visible.bounds.mins[axis]);
                maxs[axis] = maxs[axis].max(visible.bounds.maxs[axis]);
            }
        }
        if !scene::aabb_outside_clip4(mins, maxs, frustum, 0x0f) {
            continue;
        }
        census.rejected_groups = census.rejected_groups.wrapping_add(1);
        census.rejected_faces = census.rejected_faces.wrapping_add(block.len() as u32);
        for visible in block {
            census.aabb_tests_saved =
                census
                    .aabb_tests_saved
                    .wrapping_add(u32::from(face_reaches_aabb(
                        visible,
                        active_textures,
                        origin,
                        water_plane,
                    )));
        }
    }
    census
}

#[cfg(feature = "renderer-census")]
#[inline(never)]
fn census_face_blocks(
    visible_faces: &[VisibleFace],
    active_textures: &[TextureInfo],
    origin: Vec3I32,
    frustum: &[AabbClipPlane; 4],
    water_plane: i16,
) -> [BlockCensus; 3] {
    [
        census_face_blocks_for(
            visible_faces,
            active_textures,
            origin,
            frustum,
            water_plane,
            4,
        ),
        census_face_blocks_for(
            visible_faces,
            active_textures,
            origin,
            frustum,
            water_plane,
            8,
        ),
        census_face_blocks_for(
            visible_faces,
            active_textures,
            origin,
            frustum,
            water_plane,
            16,
        ),
    ]
}

#[cfg(feature = "renderer-census")]
#[inline(always)]
fn finish_projection_batch(
    census: &mut ProjectionCensus,
    batch_corners: &mut usize,
    batch_surfaces: &mut usize,
    unique_count: &mut usize,
) {
    if *batch_corners != 0 {
        census.batches = census.batches.wrapping_add(1);
        census.unique_positions = census.unique_positions.wrapping_add(*unique_count as u32);
    }
    *batch_corners = 0;
    *batch_surfaces = 0;
    *unique_count = 0;
}

/// Estimate transform reuse for a bounded selected-only projector. It mirrors
/// the ordinary 39-corner/13-surface batch limits, but deliberately flushes
/// around near-clipped and special surfaces. Packet-arena capacity can cause
/// additional shipping flushes, so this is an optimistic structural bound,
/// not a performance claim.
#[cfg(feature = "renderer-census")]
#[inline(never)]
fn census_projection_batches(
    map: &ResidentMap,
    visible_faces: &[VisibleFace],
    active_textures: &[TextureInfo],
    selected: &[u16],
    frame_light: Option<DynamicLight>,
) -> ProjectionCensus {
    let indexed = map.indexed_vertices().expect("validated PSB4 vertices");
    let mut census = ProjectionCensus::default();
    let mut unique_positions = [0u16; BATCH_MAX_VERTICES];
    let mut unique_count = 0usize;
    let mut batch_corners = 0usize;
    let mut batch_surfaces = 0usize;
    let mut previous_positions = [0u16; BATCH_MAX_VERTICES];
    let mut previous_count = 0usize;
    let mut second_previous_positions = [0u16; BATCH_MAX_VERTICES];
    let mut second_previous_count = 0usize;

    for &entry in selected {
        let visible =
            unsafe { visible_faces.get_unchecked((entry & FRAME_FACE_INDEX_MASK) as usize) };
        let texture = unsafe { active_textures.get_unchecked(visible.face.material as usize) };
        let corner_count = visible.face.corner_count as usize;
        let flags = texture.flags;
        let fallback = if flags & TEXTURE_LAYERED_SKY != 0 {
            census.layered_sky_corners =
                census.layered_sky_corners.wrapping_add(corner_count as u32);
            true
        } else if flags & (TEXTURE_LIQUID | TEXTURE_SKY) != 0 {
            census.special_corners = census.special_corners.wrapping_add(corner_count as u32);
            true
        } else if entry & NEAR_FACE_BIT != 0 {
            census.near_corners = census.near_corners.wrapping_add(corner_count as u32);
            true
        } else if corner_count > BATCH_MAX_VERTICES {
            census.oversized_corners = census.oversized_corners.wrapping_add(corner_count as u32);
            true
        } else {
            false
        };
        if fallback {
            finish_projection_batch(
                &mut census,
                &mut batch_corners,
                &mut batch_surfaces,
                &mut unique_count,
            );
            previous_count = 0;
            second_previous_count = 0;
            continue;
        }

        let root_triangles = corner_count.saturating_sub(2);
        let base_packet_bytes = ((root_triangles / 2) * 52 + (root_triangles & 1) * 40) as u32;
        census.ordinary_base_packet_bytes = census
            .ordinary_base_packet_bytes
            .wrapping_add(base_packet_bytes);
        let face_flags = u16::from(visible.face.flags);
        if face_flags & (FACE_BAKED_UV | FACE_BAKED_LIGHT) == FACE_BAKED_UV | FACE_BAKED_LIGHT {
            if frame_light.is_some_and(|light| {
                !dynamic_light_misses(light, visible.bounds.mins, visible.bounds.maxs)
            }) {
                census.dynamic_light_template_reject_bytes = census
                    .dynamic_light_template_reject_bytes
                    .wrapping_add(base_packet_bytes);
            } else {
                census.resident_template_faces = census.resident_template_faces.wrapping_add(1);
                census.resident_template_packet_bytes = census
                    .resident_template_packet_bytes
                    .wrapping_add(base_packet_bytes);
            }
        }

        if batch_corners + corner_count > BATCH_MAX_VERTICES || batch_surfaces == BATCH_MAX_SURFACES
        {
            finish_projection_batch(
                &mut census,
                &mut batch_corners,
                &mut batch_surfaces,
                &mut unique_count,
            );
            previous_count = 0;
            second_previous_count = 0;
        }

        let first = visible.face.first_corner as usize;
        let corners = &indexed.corners[first..first + corner_count];
        for corner in corners {
            let position_index = corner.position_index;
            let reused_previous = previous_positions[..previous_count]
                .iter()
                .any(|&cached| cached == position_index);
            let reused_second = second_previous_positions[..second_previous_count]
                .iter()
                .any(|&cached| cached == position_index);
            census.previous_face_reuses = census
                .previous_face_reuses
                .wrapping_add(u32::from(reused_previous));
            census.previous_two_face_reuses = census
                .previous_two_face_reuses
                .wrapping_add(u32::from(reused_previous || reused_second));
            let mut already_present = false;
            for &cached in &unique_positions[..unique_count] {
                already_present |= cached == position_index;
            }
            if !already_present {
                unique_positions[unique_count] = position_index;
                unique_count += 1;
            }
        }
        second_previous_positions[..previous_count]
            .copy_from_slice(&previous_positions[..previous_count]);
        second_previous_count = previous_count;
        for (destination, corner) in previous_positions[..corner_count]
            .iter_mut()
            .zip(corners.iter())
        {
            *destination = corner.position_index;
        }
        previous_count = corner_count;
        batch_corners += corner_count;
        batch_surfaces += 1;
        census.candidate_corners = census.candidate_corners.wrapping_add(corner_count as u32);
    }
    finish_projection_batch(
        &mut census,
        &mut batch_corners,
        &mut batch_surfaces,
        &mut unique_count,
    );
    census
}

#[cfg(feature = "renderer-census")]
fn selected_fingerprints(selected: &[u16]) -> (u32, u32) {
    let mut fnv = 0x811c_9dc5u32;
    let mut mixed = 0x9e37_79b9u32 ^ selected.len() as u32;
    for (index, &entry) in selected.iter().enumerate() {
        for byte in entry.to_le_bytes() {
            fnv = (fnv ^ u32::from(byte)).wrapping_mul(0x0100_0193);
        }
        mixed ^= u32::from(entry)
            .wrapping_add((index as u32).wrapping_mul(0x85eb_ca6b))
            .rotate_left((index & 31) as u32);
        mixed = mixed
            .rotate_left(13)
            .wrapping_mul(5)
            .wrapping_add(0xe654_6b64);
    }
    (fnv, mixed)
}

#[cfg(feature = "renderer-census")]
struct CensusLine {
    bytes: [u8; 1280],
    len: usize,
}

#[cfg(feature = "renderer-census")]
impl CensusLine {
    fn new() -> Self {
        Self {
            bytes: [0; 1280],
            len: 0,
        }
    }

    fn push_ascii(&mut self, text: &str) {
        debug_assert!(self.len + text.len() <= self.bytes.len());
        if self.len + text.len() > self.bytes.len() {
            return;
        }
        self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
    }

    fn push_field(&mut self, mut value: u32) {
        self.push_ascii(",");
        if value == 0 {
            self.push_ascii("0");
            return;
        }
        let mut digits = [0u8; 8];
        let mut count = 0usize;
        while value != 0 {
            let nibble = (value & 0x0f) as u8;
            digits[count] = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            };
            count += 1;
            value >>= 4;
        }
        while count != 0 {
            count -= 1;
            let byte = digits[count];
            let text = unsafe { core::str::from_utf8_unchecked(core::slice::from_ref(&byte)) };
            self.push_ascii(text);
        }
    }

    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }
}

/// QRC5 fields are hexadecimal and positional. Keep this order synchronized
/// with `tools/analyze_renderer_census.py`.
#[cfg(feature = "renderer-census")]
#[inline(never)]
fn emit_renderer_census(frame: u32, census: &RendererCensus) {
    let mut line = CensusLine::new();
    line.push_ascii("QRC5");
    line.push_field(frame);
    line.push_field(census.leaf);
    line.push_field(census.portal_leaf);
    line.push_field(census.visibility_rebuilt);
    line.push_field(census.selection.pvs_faces);
    line.push_field(census.selection.policy_rejects);
    line.push_field(census.selection.backface_rejects);
    line.push_field(census.selection.frustum_rejects);
    line.push_field(census.selection.selected_faces);
    line.push_field(census.near_faces);
    line.push_field(census.selection.water_blend_faces);
    line.push_field(census.selection.plane_tests);
    line.push_field(census.selection.plane_run_tests);
    line.push_field(census.selection.plane_tests_saved);
    line.push_field(census.selection.max_plane_run);
    line.push_field(
        census
            .selection
            .frustum_rejects
            .wrapping_add(census.selection.selected_faces),
    );
    for block in census.blocks {
        line.push_field(block.groups);
        line.push_field(block.rejected_groups);
        line.push_field(block.rejected_faces);
        line.push_field(block.aabb_tests_saved);
    }
    line.push_field(census.projection.candidate_corners);
    line.push_field(census.projection.unique_positions);
    line.push_field(census.projection.batches);
    line.push_field(census.projection.previous_face_reuses);
    line.push_field(census.projection.previous_two_face_reuses);
    line.push_field(census.projection.near_corners);
    line.push_field(census.projection.special_corners);
    line.push_field(census.projection.layered_sky_corners);
    line.push_field(census.projection.oversized_corners);
    line.push_field(census.projection.ordinary_base_packet_bytes);
    line.push_field(census.projection.resident_template_faces);
    line.push_field(census.projection.resident_template_packet_bytes);
    line.push_field(census.projection.dynamic_light_template_reject_bytes);
    line.push_field(census.ordinary_output_packet_bytes);
    line.push_field(census.ordinary_output_packets);
    line.push_field(census.ordinary_output_hardware_triangles);
    line.push_field(census.topology.surfaces);
    line.push_field(census.topology.root_triangles);
    line.push_field(census.topology.surface_clip_rejects);
    line.push_field(census.topology.depth_rejects);
    line.push_field(census.topology.level0_root_triangles);
    line.push_field(census.topology.level1_root_triangles);
    line.push_field(census.topology.level2_root_triangles);
    line.push_field(census.topology.paired_level0_packets);
    line.push_field(census.topology.level1_underdraw_roots);
    line.push_field(census.topology.level2_underdraw_roots);
    line.push_field(census.topology.theoretical_packets);
    line.push_field(census.topology.theoretical_hardware_triangles);
    line.push_field(census.topology.theoretical_packet_bytes);
    line.push_field(census.topology.topology_hash_a);
    line.push_field(census.topology.topology_hash_b);
    for cache in census.subdivision_slab_caches {
        line.push_field(cache.requests);
        line.push_field(cache.hits);
        line.push_field(cache.allocations);
        line.push_field(cache.replacements);
        line.push_field(cache.fallbacks);
        line.push_field(cache.resident);
        line.push_field(cache.requested_packet_bytes);
        line.push_field(cache.hit_packet_bytes);
        line.push_field(cache.hit_invariant_bytes);
    }
    line.push_field(census.packet_arena_words);
    line.push_field(census.emitted_packets);
    line.push_field(census.hardware_triangles);
    line.push_field(census.packet_overflow_avoided);
    line.push_field(census.selected_hash_a);
    line.push_field(census.selected_hash_b);
    psx_telemetry::emit::debug_log(line.as_str());
}

#[cfg(all(
    not(feature = "renderer-census"),
    not(feature = "renderer-aabb-support-offsets"),
    not(feature = "renderer-block-frustum")
))]
#[inline(never)]
fn select_frame_faces(
    visible_faces: &[VisibleFace],
    active_textures: &[TextureInfo],
    origin: Vec3I32,
    frustum: &[AabbClipPlane; 4],
    water_plane: i16,
    output: &mut Vec<u16>,
) {
    output.clear();
    debug_assert!(output.capacity() >= visible_faces.len());
    let out = output.as_mut_ptr();
    let mut count = 0usize;
    #[cfg(feature = "renderer-plane-run-cache")]
    let mut plane_cache = FrontFacingCache::EMPTY;
    let mut visible_index = 0usize;
    while visible_index < visible_faces.len() {
        let visible = unsafe { visible_faces.get_unchecked(visible_index) };
        let texture = unsafe { active_textures.get_unchecked(visible.face.material as usize) };
        let water_blend =
            texture.flags & TEXTURE_LIQUID != 0 && visible.face.plane as i16 == water_plane;
        if texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) == 0
            && (water_blend || {
                #[cfg(feature = "renderer-plane-run-cache")]
                {
                    plane_cache.test(visible, origin)
                }
                #[cfg(not(feature = "renderer-plane-run-cache"))]
                {
                    front_facing_compact_plane(visible.plane, u16::from(visible.face.flags), origin)
                }
            })
            && !scene::aabb_outside_clip4(visible.bounds.mins, visible.bounds.maxs, frustum, 0x0f)
        {
            let entry = visible_index as u16 | if water_blend { WATER_BLEND_FACE_BIT } else { 0 };
            unsafe { ptr::write(out.add(count), entry) };
            count += 1;
        }
        visible_index += 1;
    }
    unsafe { output.set_len(count) };
}

/// Exact selector variant with the AABB support-point decisions hoisted once
/// per camera. The policy, backface test, four GTE plane dots, output order,
/// and water marker are otherwise identical to [`select_frame_faces`].
#[cfg(all(
    not(feature = "renderer-census"),
    feature = "renderer-aabb-support-offsets",
    not(feature = "renderer-block-frustum")
))]
#[inline(never)]
fn select_frame_faces_preselected(
    visible_faces: &[VisibleFace],
    active_textures: &[TextureInfo],
    origin: Vec3I32,
    frustum: &[AabbClipPlane; 4],
    supports: &scene::AabbClip4SupportOffsets,
    water_plane: i16,
    output: &mut Vec<u16>,
) {
    output.clear();
    debug_assert!(output.capacity() >= visible_faces.len());
    let out = output.as_mut_ptr();
    let mut count = 0usize;
    #[cfg(feature = "renderer-plane-run-cache")]
    let mut plane_cache = FrontFacingCache::EMPTY;
    let mut visible_index = 0usize;
    while visible_index < visible_faces.len() {
        let visible = unsafe { visible_faces.get_unchecked(visible_index) };
        let texture = unsafe { active_textures.get_unchecked(visible.face.material as usize) };
        let water_blend =
            texture.flags & TEXTURE_LIQUID != 0 && visible.face.plane as i16 == water_plane;
        if texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) == 0
            && (water_blend || {
                #[cfg(feature = "renderer-plane-run-cache")]
                {
                    plane_cache.test(visible, origin)
                }
                #[cfg(not(feature = "renderer-plane-run-cache"))]
                {
                    front_facing_compact_plane(visible.plane, u16::from(visible.face.flags), origin)
                }
            })
            && !unsafe {
                scene::aabb_outside_clip4_preselected(
                    visible.bounds.mins.as_ptr(),
                    frustum,
                    supports,
                )
            }
        {
            let entry = visible_index as u16 | if water_blend { WATER_BLEND_FACE_BIT } else { 0 };
            unsafe { ptr::write(out.add(count), entry) };
            count += 1;
        }
        visible_index += 1;
    }
    unsafe { output.set_len(count) };
}

/// Exact selector with one conservative union-frustum test before each 16
/// consecutive PVS faces. Rejected blocks cannot contain a selected face;
/// accepted blocks retain the authoritative per-face policy and output order.
#[cfg(all(not(feature = "renderer-census"), feature = "renderer-block-frustum"))]
#[inline(never)]
fn select_frame_faces_blocked(
    visible_faces: &[VisibleFace],
    #[cfg(feature = "renderer-compact-cell-stream")] visible_planes: &[CompactPlane],
    visible_blocks: &[VisibleFaceBlock],
    #[cfg(not(feature = "renderer-cell-liquid-policy"))] active_textures: &[TextureInfo],
    origin: Vec3I32,
    frustum: &[AabbClipPlane; 4],
    water_plane: i16,
    output: &mut Vec<u16>,
) {
    output.clear();
    debug_assert!(output.capacity() >= visible_faces.len());
    #[cfg(feature = "renderer-compact-cell-stream")]
    debug_assert_eq!(visible_faces.len(), visible_planes.len());
    debug_assert_eq!(
        visible_blocks.len(),
        visible_faces.len().div_ceil(VISIBLE_FACE_BLOCK_SIZE)
    );
    let out = output.as_mut_ptr();
    let mut count = 0usize;
    #[cfg(feature = "renderer-plane-run-cache")]
    let mut plane_cache = FrontFacingCache::EMPTY;
    let mut block_index = 0usize;
    let mut first = 0usize;
    while first < visible_faces.len() {
        let block = unsafe { visible_blocks.get_unchecked(block_index) };
        let end = (first + VISIBLE_FACE_BLOCK_SIZE).min(visible_faces.len());
        #[cfg(feature = "renderer-block-clip-flags")]
        let block_clip_flags = scene::classify_aabb_clip4(block.mins, block.maxs, frustum, 0x0f);
        #[cfg(not(feature = "renderer-block-clip-flags"))]
        let block_clip_flags = if scene::aabb_outside_clip4(block.mins, block.maxs, frustum, 0x0f) {
            -1
        } else {
            0x0f
        };
        if block_clip_flags >= 0 {
            let mut visible_index = first;
            while visible_index < end {
                let visible = unsafe { visible_faces.get_unchecked(visible_index) };
                #[cfg(not(feature = "renderer-cell-liquid-policy"))]
                let texture =
                    unsafe { active_textures.get_unchecked(visible.face.material as usize) };
                #[cfg(not(feature = "renderer-cell-liquid-policy"))]
                let water_blend =
                    texture.flags & TEXTURE_LIQUID != 0 && visible.face.plane as i16 == water_plane;
                #[cfg(feature = "renderer-cell-liquid-policy")]
                let water_blend = visible.bounds.surface_index & VISIBLE_LIQUID_BIT != 0
                    && visible.face.plane as i16 == water_plane;
                #[cfg(any(
                    feature = "renderer-compact-cell-stream",
                    feature = "renderer-cell-policy"
                ))]
                let policy_visible = true;
                #[cfg(not(any(
                    feature = "renderer-compact-cell-stream",
                    feature = "renderer-cell-policy"
                )))]
                let policy_visible = texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) == 0;
                if policy_visible
                    && (water_blend || {
                        #[cfg(any(
                            feature = "renderer-compact-cell-stream",
                            feature = "renderer-cell-policy"
                        ))]
                        {
                            visible.bounds.surface_index & VISIBLE_INVARIANT_FRONT_BIT != 0
                                || front_facing_compact_plane(
                                    #[cfg(feature = "renderer-compact-cell-stream")]
                                    unsafe { *visible_planes.get_unchecked(visible_index) },
                                    #[cfg(all(
                                        feature = "renderer-cell-policy",
                                        not(feature = "renderer-compact-cell-stream")
                                    ))]
                                    visible.plane,
                                    u16::from(visible.face.flags),
                                    origin,
                                )
                        }
                        #[cfg(all(
                            not(any(
                                feature = "renderer-compact-cell-stream",
                                feature = "renderer-cell-policy"
                            )),
                            feature = "renderer-plane-run-cache"
                        ))]
                        {
                            plane_cache.test(visible, origin)
                        }
                        #[cfg(all(
                            not(any(
                                feature = "renderer-compact-cell-stream",
                                feature = "renderer-cell-policy"
                            )),
                            not(feature = "renderer-plane-run-cache")
                        ))]
                        {
                            front_facing_compact_plane(
                                visible.plane,
                                u16::from(visible.face.flags),
                                origin,
                            )
                        }
                    })
                    && !scene::aabb_outside_clip4(
                        visible.bounds.mins,
                        visible.bounds.maxs,
                        frustum,
                        block_clip_flags as u8,
                    )
                {
                    let entry =
                        visible_index as u16 | if water_blend { WATER_BLEND_FACE_BIT } else { 0 };
                    unsafe { ptr::write(out.add(count), entry) };
                    count += 1;
                }
                visible_index += 1;
            }
        }
        first = end;
        block_index += 1;
    }
    unsafe { output.set_len(count) };
}

/// Block-frustum selector with an exact direct-index memo of the camera side
/// of every BSP plane touched this frame. Faces keep source order and their
/// own backside flag; only repeated `normal.dot(origin)-distance` arithmetic
/// is removed.
#[cfg(all(
    not(feature = "renderer-census"),
    feature = "renderer-block-frustum",
    feature = "renderer-plane-index-cache",
    not(feature = "renderer-hierarchical-block-frustum")
))]
#[inline(never)]
fn select_frame_faces_blocked_plane_indexed(
    visible_faces: &[VisibleFace],
    visible_blocks: &[VisibleFaceBlock],
    active_textures: &[TextureInfo],
    origin: Vec3I32,
    frustum: &[AabbClipPlane; 4],
    water_plane: i16,
    epoch: u16,
    plane_stamps: &mut [u16],
    plane_behind: &mut [u8],
    output: &mut Vec<u16>,
) {
    output.clear();
    debug_assert!(output.capacity() >= visible_faces.len());
    debug_assert_eq!(plane_stamps.len(), plane_behind.len());
    debug_assert_eq!(
        visible_blocks.len(),
        visible_faces.len().div_ceil(VISIBLE_FACE_BLOCK_SIZE)
    );
    let out = output.as_mut_ptr();
    let stamps = plane_stamps.as_mut_ptr();
    let behind_values = plane_behind.as_mut_ptr();
    let mut count = 0usize;
    let mut block_index = 0usize;
    let mut first = 0usize;
    while first < visible_faces.len() {
        let block = unsafe { visible_blocks.get_unchecked(block_index) };
        let end = (first + VISIBLE_FACE_BLOCK_SIZE).min(visible_faces.len());
        if !scene::aabb_outside_clip4(block.mins, block.maxs, frustum, 0x0f) {
            let mut visible_index = first;
            while visible_index < end {
                let visible = unsafe { visible_faces.get_unchecked(visible_index) };
                let texture =
                    unsafe { active_textures.get_unchecked(visible.face.material as usize) };
                let water_blend =
                    texture.flags & TEXTURE_LIQUID != 0 && visible.face.plane as i16 == water_plane;
                let plane_index = visible.face.plane as usize;
                debug_assert!(plane_index < plane_stamps.len());
                let facing = if water_blend {
                    true
                } else {
                    let stamp = unsafe { ptr::read(stamps.add(plane_index)) };
                    let behind = if stamp == epoch {
                        unsafe { ptr::read(behind_values.add(plane_index)) != 0 }
                    } else {
                        let behind = compact_plane_distance(visible.plane, origin) < 0;
                        unsafe {
                            ptr::write(stamps.add(plane_index), epoch);
                            ptr::write(behind_values.add(plane_index), u8::from(behind));
                        }
                        behind
                    };
                    behind == (u16::from(visible.face.flags) & FACE_BACKSIDE != 0)
                };
                if texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) == 0
                    && facing
                    && !scene::aabb_outside_clip4(
                        visible.bounds.mins,
                        visible.bounds.maxs,
                        frustum,
                        0x0f,
                    )
                {
                    let entry =
                        visible_index as u16 | if water_blend { WATER_BLEND_FACE_BIT } else { 0 };
                    unsafe { ptr::write(out.add(count), entry) };
                    count += 1;
                }
                visible_index += 1;
            }
        }
        first = end;
        block_index += 1;
    }
    unsafe { output.set_len(count) };
}

/// Exact two-level selector inspired by Quake II's doorway-before-brush
/// funnel. A conservative union covers four consecutive 16-face blocks. A
/// rejected super-block skips all of its descendants; an admitted one runs
/// the established block and per-face tests without changing output order.
#[cfg(all(
    not(feature = "renderer-census"),
    feature = "renderer-hierarchical-block-frustum"
))]
#[inline(never)]
fn select_frame_faces_hierarchical(
    visible_faces: &[VisibleFace],
    visible_blocks: &[VisibleFaceBlock],
    visible_super_blocks: &[VisibleFaceBlock],
    active_textures: &[TextureInfo],
    origin: Vec3I32,
    frustum: &[AabbClipPlane; 4],
    water_plane: i16,
    output: &mut Vec<u16>,
) {
    output.clear();
    debug_assert!(output.capacity() >= visible_faces.len());
    debug_assert_eq!(
        visible_blocks.len(),
        visible_faces.len().div_ceil(VISIBLE_FACE_BLOCK_SIZE)
    );
    debug_assert_eq!(
        visible_super_blocks.len(),
        visible_blocks.len().div_ceil(VISIBLE_FACE_SUPER_BLOCK_SIZE)
    );
    let out = output.as_mut_ptr();
    let mut count = 0usize;
    #[cfg(feature = "renderer-plane-run-cache")]
    let mut plane_cache = FrontFacingCache::EMPTY;
    let mut block_index = 0usize;
    let mut super_index = 0usize;
    while super_index < visible_super_blocks.len() {
        let super_block = unsafe { visible_super_blocks.get_unchecked(super_index) };
        let block_end = (block_index + VISIBLE_FACE_SUPER_BLOCK_SIZE).min(visible_blocks.len());
        if !scene::aabb_outside_clip4(super_block.mins, super_block.maxs, frustum, 0x0f) {
            while block_index < block_end {
                let block = unsafe { visible_blocks.get_unchecked(block_index) };
                let first = block_index * VISIBLE_FACE_BLOCK_SIZE;
                let end = (first + VISIBLE_FACE_BLOCK_SIZE).min(visible_faces.len());
                if !scene::aabb_outside_clip4(block.mins, block.maxs, frustum, 0x0f) {
                    let mut visible_index = first;
                    while visible_index < end {
                        let visible = unsafe { visible_faces.get_unchecked(visible_index) };
                        let texture = unsafe {
                            active_textures.get_unchecked(visible.face.material as usize)
                        };
                        let water_blend = texture.flags & TEXTURE_LIQUID != 0
                            && visible.face.plane as i16 == water_plane;
                        if texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) == 0
                            && (water_blend || {
                                #[cfg(feature = "renderer-plane-run-cache")]
                                {
                                    plane_cache.test(visible, origin)
                                }
                                #[cfg(not(feature = "renderer-plane-run-cache"))]
                                {
                                    front_facing_compact_plane(
                                        visible.plane,
                                        u16::from(visible.face.flags),
                                        origin,
                                    )
                                }
                            })
                            && !scene::aabb_outside_clip4(
                                visible.bounds.mins,
                                visible.bounds.maxs,
                                frustum,
                                0x0f,
                            )
                        {
                            let entry = visible_index as u16
                                | if water_blend { WATER_BLEND_FACE_BIT } else { 0 };
                            unsafe { ptr::write(out.add(count), entry) };
                            count += 1;
                        }
                        visible_index += 1;
                    }
                }
                block_index += 1;
            }
        } else {
            block_index = block_end;
        }
        super_index += 1;
    }
    unsafe { output.set_len(count) };
}

/// Set [`NEAR_FACE_BIT`] on every selected face whose bounds reach behind
/// the near plane. A separate pass over the (short) selected list rather than
/// part of `select_frame_faces`, whose loop is at the register limit: folded
/// in there the plane spilled the frustum selectors to the stack.
#[inline(never)]
fn flag_near_faces(visible_faces: &[VisibleFace], selected: &mut [u16], near: NearPlane) {
    for entry in selected.iter_mut() {
        let visible =
            unsafe { visible_faces.get_unchecked((*entry & FRAME_FACE_INDEX_MASK) as usize) };
        if near.reaches_behind(visible.bounds.mins, visible.bounds.maxs) {
            *entry |= NEAR_FACE_BIT;
        }
    }
}

/// Preserve the separate, register-light selected-face pass while moving its
/// three signed products to the GTE row already loaded beside the frustum.
#[cfg(feature = "renderer-gte-near-classification")]
#[inline(never)]
fn flag_near_faces_gte(visible_faces: &[VisibleFace], selected: &mut [u16], near: &AabbClipPlane) {
    for entry in selected.iter_mut() {
        let visible =
            unsafe { visible_faces.get_unchecked((*entry & FRAME_FACE_INDEX_MASK) as usize) };
        if aabb_reaches_behind_near_gte(visible.bounds.mins, visible.bounds.maxs, near) {
            *entry |= NEAR_FACE_BIT;
        }
    }
}

/// Load the four ordinary frustum planes plus the near plane used by the
/// separate selected-face pass. Keeping this composition local means the
/// renderer builds reproducibly against the pinned public PSoXide API.
#[cfg(feature = "renderer-gte-near-classification")]
#[inline(always)]
fn load_aabb_clip4_with_near(planes: &[AabbClipPlane; 4], near: &AabbClipPlane) {
    scene::load_rotation(&Mat3I16 {
        m: [planes[0].normal, planes[1].normal, planes[2].normal],
    });
    scene::load_light_matrix(&Mat3I16 {
        m: [planes[3].normal, near.normal, [0; 3]],
    });
}

/// Test the AABB's inner support point against the near plane retained in the
/// second light-matrix row. The MIPS path mirrors PSoXide's public four-plane
/// classifier schedule, selecting MAC2 after the light-matrix MVMVA.
#[cfg(feature = "renderer-gte-near-classification")]
#[inline(always)]
fn aabb_reaches_behind_near_gte(
    mins: [i16; 3],
    maxs: [i16; 3],
    near: &AabbClipPlane,
) -> bool {
    let inner = GteVec3I16::new(
        if near.signbits & 1 != 0 {
            maxs[0]
        } else {
            mins[0]
        },
        if near.signbits & 2 != 0 {
            maxs[1]
        } else {
            mins[1]
        },
        if near.signbits & 4 != 0 {
            maxs[2]
        } else {
            mins[2]
        },
    );

    #[cfg(target_arch = "mips")]
    let dot = {
        let mut dot = inner.xy_packed();
        unsafe {
            core::arch::asm!(
                ".word 0x48880000",
                ".word 0x48890800",
                ".word 0",
                ".word 0",
                // MVMVA using the light matrix, followed by MAC2.
                ".word 0x4a026012",
                ".word 0x4808d000",
                ".word 0",
                inlateout("$8") dot,
                in("$9") inner.z_packed(),
                options(nostack, nomem, preserves_flags),
            );
        }
        dot as i32
    };

    #[cfg(not(target_arch = "mips"))]
    let dot = i32::from(near.normal[0])
        .wrapping_mul(i32::from(inner.x))
        .wrapping_add(i32::from(near.normal[1]).wrapping_mul(i32::from(inner.y)))
        .wrapping_add(i32::from(near.normal[2]).wrapping_mul(i32::from(inner.z)));

    dot < near.distance
}

/// Quake near plane backed by the face's cached transformed depths.
struct QuakeNearClipPlane<'a> {
    depths: &'a [i32],
}

impl AttributedClipPlane<ClassicAffineVertex> for QuakeNearClipPlane<'_> {
    type Distance = i32;

    #[inline(always)]
    fn distance(&self, source_index: usize, _: &ClassicAffineVertex) -> Self::Distance {
        unsafe { *self.depths.get_unchecked(source_index) }
    }

    #[inline(always)]
    fn inside(&self, distance: Self::Distance) -> bool {
        distance >= NEAR_PLANE_VIEW
    }

    #[inline(always)]
    fn intersection(
        &self,
        _: usize,
        first: &ClassicAffineVertex,
        first_distance: Self::Distance,
        _: usize,
        second: &ClassicAffineVertex,
        second_distance: Self::Distance,
    ) -> ClassicAffineVertex {
        let fraction =
            ((NEAR_PLANE_VIEW - first_distance) << 12) / (second_distance - first_distance);
        lerp_vertex(*first, *second, fraction)
    }
}

/// Clip a materialized convex face against the near plane in view space,
/// rewriting `vertices` in place and returning the new count (at most
/// `count + 1`, so the slot must have room for one more record). Positions
/// stay in the space the loaded GTE matrix expects, so the clipped fan goes
/// through the ordinary projection, subdivision and packet path.
///
/// # Safety
/// `vertices` must point to `count + 1` writable records, and the GTE must
/// hold the rotation and translation this face is submitted with.
unsafe fn clip_face_near(vertices: *mut ClassicAffineVertex, count: usize) -> usize {
    debug_assert!(count <= NEAR_CLIP_MAX_VERTICES);
    let mut depth = [MaybeUninit::<i32>::uninit(); NEAR_CLIP_MAX_VERTICES];
    let mut inside = 0usize;
    for index in 0..count {
        let position = unsafe { (*vertices.add(index)).position };
        let z = scene::transform_vertex_scheduled(GteVec3I16::new(
            position[0],
            position[1],
            position[2],
        ))
        .z;
        depth[index].write(z);
        inside += usize::from(z >= NEAR_PLANE_VIEW);
    }
    if inside == count {
        return count;
    }
    if inside == 0 {
        return 0;
    }
    // Every entry below `count` was written above. Keep Quake's cached
    // transformed depths authoritative in the shared traversal.
    let depths = unsafe { core::slice::from_raw_parts(depth.as_ptr().cast::<i32>(), count) };
    let source = unsafe { core::slice::from_raw_parts(vertices, count) };
    let mut clipped = [MaybeUninit::<ClassicAffineVertex>::uninit(); NEAR_CLIP_MAX_VERTICES + 1];
    // Sutherland-Hodgman against one plane keeps the winding and adds at most
    // one vertex on a convex polygon. Uninitialised output retains the old
    // zero-clear cost rather than paying for an abstraction-friendly buffer.
    let written = unsafe {
        clip_convex_plane_uninit::<_, _, true>(
            source,
            &mut clipped[..count + 1],
            &QuakeNearClipPlane { depths },
            ClipTraversal::CurrentToNext,
        )
    };
    for index in 0..written {
        // Every entry below `written` was written above.
        unsafe { ptr::write(vertices.add(index), clipped[index].assume_init()) };
    }
    written
}

/// Interpolate position, UV and colour by the Q12 fraction `t` from `a` to
/// `b`, rounding to nearest. UV bytes are linear across a face (the GPU
/// interpolates them the same way), so no wrap handling is wanted here.
#[inline(always)]
fn lerp_vertex(a: ClassicAffineVertex, b: ClassicAffineVertex, t: i32) -> ClassicAffineVertex {
    let mix = |from: i32, to: i32| lerp_q12_i32_rounded(from, to, t);
    let channel = |shift: u32| {
        (mix(
            ((a.color >> shift) & 0xff) as i32,
            ((b.color >> shift) & 0xff) as i32,
        ) as u32)
            << shift
    };
    ClassicAffineVertex {
        position: [
            mix(i32::from(a.position[0]), i32::from(b.position[0])) as i16,
            mix(i32::from(a.position[1]), i32::from(b.position[1])) as i16,
            mix(i32::from(a.position[2]), i32::from(b.position[2])) as i16,
        ],
        uv: [
            mix(i32::from(a.uv[0]), i32::from(b.uv[0])) as u8,
            mix(i32::from(a.uv[1]), i32::from(b.uv[1])) as u8,
        ],
        color: channel(0) | channel(8) | channel(16),
        screen: [0; 2],
        depth: 0,
    }
}

/// Convert the shared scoped-window fan's opaque textured-Gouraud packets to
/// their PS1 semitransparent opcodes in place. Every packet starts with an OT
/// tag, then GP0(E2), then the color/command word; bit 25 distinguishes
/// 34h/3Ch from 36h/3Eh without changing packet length or topology.
#[optimize(size)]
unsafe fn mark_window_packets_translucent(mut packet: *mut u32, end: *mut u32) {
    while packet < end {
        let data_words = unsafe { ptr::read(packet) } >> 24;
        let command = unsafe { packet.add(2) };
        unsafe { ptr::write(command, ptr::read(command) | 0x0200_0000) };
        packet = unsafe { packet.add(data_words as usize + 1) };
    }
}

#[inline]
fn front_facing_plane(plane: Plane, face_flags: u16, point: Vec3I32) -> bool {
    let behind = plane_distance(plane, point) < 0;
    behind == (face_flags & FACE_BACKSIDE != 0)
}

#[inline]
fn front_facing_compact_plane(plane: CompactPlane, face_flags: u16, point: Vec3I32) -> bool {
    let behind = compact_plane_distance(plane, point) < 0;
    behind == (face_flags & FACE_BACKSIDE != 0)
}

/// Return the exact facing result when a supporting plane cannot cross the
/// outward-quantized source-leaf AABB. This is a cold leaf-transition test.
#[cfg(any(
    feature = "renderer-compact-cell-stream",
    feature = "renderer-cell-policy"
))]
fn leaf_invariant_facing(
    plane: CompactPlane,
    face_flags: u16,
    bounds: quake_formats::LeafBounds,
) -> Option<bool> {
    let normal = [plane.normal.x, plane.normal.y, plane.normal.z];
    let mut minimum = 0i32;
    let mut maximum = 0i32;
    for axis in 0..3 {
        let (near, far) = if normal[axis] < 0 {
            (bounds.maxs[axis], bounds.mins[axis])
        } else {
            (bounds.mins[axis], bounds.maxs[axis])
        };
        minimum += i32::from(near) * i32::from(normal[axis]);
        maximum += i32::from(far) * i32::from(normal[axis]);
    }
    minimum -= plane.distance;
    maximum -= plane.distance;
    let behind = if maximum < 0 {
        true
    } else if minimum >= 0 {
        false
    } else {
        return None;
    };
    Some(behind == (face_flags & FACE_BACKSIDE != 0))
}

/// One-entry exact memo for the camera-side result of a BSP plane. Cooked
/// visible faces are kept in source order, where adjacent faces commonly
/// share both the plane index and `FACE_BACKSIDE` side. The camera is fixed
/// for the whole selection pass, so those tests are mathematically identical.
#[cfg(feature = "renderer-plane-run-cache")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct FrontFacingCache {
    key: u32,
    value: bool,
}

#[cfg(feature = "renderer-plane-run-cache")]
impl FrontFacingCache {
    const EMPTY: Self = Self {
        key: u32::MAX,
        value: false,
    };

    #[inline(always)]
    fn test(&mut self, visible: &VisibleFace, point: Vec3I32) -> bool {
        let side = u32::from(u16::from(visible.face.flags) & FACE_BACKSIDE);
        let key = u32::from(visible.face.plane) | side << 16;
        if self.key != key {
            self.key = key;
            self.value =
                front_facing_compact_plane(visible.plane, u16::from(visible.face.flags), point);
        }
        self.value
    }
}

/// Signed Q20.12 distance of `point` from `plane`. World points against unit
/// normals keep every term inside `i32`, so the wide multiply and wrapping
/// adds equal the saturating form here at a fraction of its instructions
/// (this runs once per PVS face per frame).
#[inline(always)]
fn plane_distance(plane: Plane, point: Vec3I32) -> i32 {
    let dot = match plane.kind {
        0 => point.x,
        1 => point.y,
        2 => point.z,
        _ => mul_q12_i32_wide(point.x, plane.normal.x as i32)
            .wrapping_add(mul_q12_i32_wide(point.y, plane.normal.y as i32))
            .wrapping_add(mul_q12_i32_wide(point.z, plane.normal.z as i32)),
    };
    dot.wrapping_sub(plane.distance)
}

#[inline(always)]
fn compact_plane_distance(plane: CompactPlane, point: Vec3I32) -> i32 {
    let dot = match plane.kind {
        0 => point.x,
        1 => point.y,
        2 => point.z,
        _ => mul_q12_i32_wide(point.x, plane.normal.x as i32)
            .wrapping_add(mul_q12_i32_wide(point.y, plane.normal.y as i32))
            .wrapping_add(mul_q12_i32_wide(point.z, plane.normal.z as i32)),
    };
    dot.wrapping_sub(plane.distance)
}

fn decompress_visibility(input: &[u8], offset: usize, output: &mut [u8]) -> bool {
    let mut source = offset;
    let mut destination = 0usize;
    while destination < output.len() {
        let Some(&value) = input.get(source) else {
            return false;
        };
        source += 1;
        if value != 0 {
            output[destination] = value;
            destination += 1;
            continue;
        }
        let Some(&run) = input.get(source) else {
            return false;
        };
        source += 1;
        if run == 0 || destination + run as usize > output.len() {
            return false;
        }
        output[destination..destination + run as usize].fill(0);
        destination += run as usize;
    }
    true
}

/// OR one Quake RLE visibility row into an already decompressed row without a
/// third scratch buffer. Zero runs only advance the destination.
#[optimize(size)]
fn merge_visibility(input: &[u8], offset: usize, output: &mut [u8]) -> bool {
    let mut source = offset;
    let mut destination = 0usize;
    while destination < output.len() {
        let Some(&value) = input.get(source) else {
            return false;
        };
        source += 1;
        if value != 0 {
            output[destination] |= value;
            destination += 1;
            continue;
        }
        let Some(&run) = input.get(source) else {
            return false;
        };
        source += 1;
        if run == 0 || destination + run as usize > output.len() {
            return false;
        }
        destination += run as usize;
    }
    true
}
