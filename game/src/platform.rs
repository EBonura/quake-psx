use core::mem::MaybeUninit;
use core::ptr::{addr_of, addr_of_mut};

use psx_gpu::display::{DoubleBuffer, Resolution};
use psx_gpu::material::TextureMaterial;
use psx_gpu::ot::OrderingTable;
use psx_rt::present::{Part, Presenter};
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::scene;
use psx_pack::cd::{find_entry, SectorReader, SECTOR_WORDS, WORLD_PACK_DEFAULT_LBA};
use psx_pack::{parse_entry, parse_header};
use psx_pack::{PackEntry, SECTOR_BYTES};
use psx_vram::VramRect;

const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;
const BACK_BUFFER_Y: u16 = 256;

const OT_DEPTH: usize = 2048;

// Menu and HUD glyphs share this late-command list. On PS1 the metadata is
// three KiB and packet storage remains double-buffered in the renderer.
const MAX_SCREEN_COMMANDS: usize = 384;

// Two sectors keep small Quake reads coalesced while leaving the largest
// Episode 1 map enough resident heap beside the double-buffered GPU arenas.
// The regression harness builds carry ~68 KB more code than the shipping
// game and sat within 2 KB of the bump allocator's end at eight sectors,
// which surfaced as a silent halt at boot (the OOM panic never flushes).
// Two is the floor: the cached path below admits reads of half the cache,
// and a half-cache read straddles two sectors at worst only while the cache
// spans at least two.
const STORAGE_CACHE_SECTORS: usize = 2;
const STORAGE_CACHE_BYTES: usize = STORAGE_CACHE_SECTORS * SECTOR_BYTES;

static mut OTS: [OrderingTable<OT_DEPTH>; 2] = [OrderingTable::new(), OrderingTable::new()];
static mut BUFFERS: DoubleBuffer = DoubleBuffer::with_stride(Resolution::R320X240, BACK_BUFFER_Y);
/// The frame path's one presenter, made by [`start_vblank_counter`].
static mut PRESENTER: MaybeUninit<Presenter> = MaybeUninit::uninit();
static mut SCREEN_COMMANDS: [usize; MAX_SCREEN_COMMANDS * 2] = [0; MAX_SCREEN_COMMANDS * 2];
static mut SCREEN_COMMAND_COUNT: usize = 0;

static mut BUILD_BUFFER: usize = 0;

// SAFETY: the one SectorReader of the program; the token is a logic guard.
static mut READER: SectorReader = SectorReader::with_cd(unsafe { psx_io::periph::Cd::steal() });
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
static mut PACK_TABLE: [TableEntry; PACK_TABLE_CAP] = [TableEntry::EMPTY; PACK_TABLE_CAP];
static mut PACK_TABLE_LEN: usize = 0;
static mut STORAGE_CACHE: [u32; SECTOR_WORDS * STORAGE_CACHE_SECTORS] =
    [0; SECTOR_WORDS * STORAGE_CACHE_SECTORS];
static mut STORAGE_CACHE_CHUNK: u32 = u32::MAX;
static mut STORAGE_CACHE_OFFSET: u32 = 0;
static mut STORAGE_CACHE_LEN: usize = 0;
static mut STORAGE_STREAM_ACTIVE: bool = false;

#[cfg(feature = "emulator-telemetry")]
static mut TELEMETRY_FRAME: u32 = 0;

#[cfg(feature = "hardware-performance")]
const HARDWARE_PERF_MAGIC: u32 = 0x5057_4851;
#[cfg(all(feature = "hardware-performance", not(feature = "hardware-regression")))]
const HARDWARE_PERF_SAMPLE_FRAMES: u32 = 600;

/// Direct-Rust snapshot of presentation cadence and GPU back-pressure.
#[cfg(feature = "hardware-performance")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HardwarePerformance {
    pub magic: u32,
    pub version: u32,
    pub samples: u32,
    pub total_vblanks: u32,
    pub one_vblank: u32,
    pub two_vblank: u32,
    pub three_plus_vblank: u32,
    pub max_vblanks: u32,
    pub gpu_wait_events: u32,
    pub gpu_wait_vblanks: u32,
    pub gpu_wait_max_vblanks: u32,
    pub active: bool,
    pub complete: bool,
}

#[cfg(feature = "hardware-performance")]
impl HardwarePerformance {
    const fn new() -> Self {
        Self {
            magic: HARDWARE_PERF_MAGIC,
            version: 2,
            samples: 0,
            total_vblanks: 0,
            one_vblank: 0,
            two_vblank: 0,
            three_plus_vblank: 0,
            max_vblanks: 0,
            gpu_wait_events: 0,
            gpu_wait_vblanks: 0,
            gpu_wait_max_vblanks: 0,
            active: true,
            complete: false,
        }
    }
}

#[cfg(feature = "hardware-performance")]
static mut HARDWARE_PERF: HardwarePerformance = HardwarePerformance::new();
#[cfg(feature = "hardware-performance")]
static mut HARDWARE_PERF_LAST_PRESENT: u32 = 0;
#[cfg(feature = "hardware-performance")]
static mut HARDWARE_PERF_HAS_PRESENT: bool = false;

#[optimize(size)]
fn configure_world_material(gpu: &mut psx_gpu::Gpu) {
    gpu.set_draw_mode(psx_gpu::material::TextureMaterial::new(0, 0).with_dither(true));
}

unsafe fn build_ot() -> &'static mut OrderingTable<OT_DEPTH> {
    unsafe {
        &mut *addr_of_mut!(OTS)
            .cast::<OrderingTable<OT_DEPTH>>()
            .add(BUILD_BUFFER)
    }
}

unsafe fn buffers() -> &'static mut DoubleBuffer {
    unsafe { &mut *addr_of_mut!(BUFFERS) }
}

/// The frame path's presenter.
///
/// # Safety
/// [`start_vblank_counter`] has run, and nothing else holds the presenter.
unsafe fn presenter() -> &'static mut Presenter {
    unsafe { (*addr_of_mut!(PRESENTER)).assume_init_mut() }
}

// Every frame goes to the screen through psx-rt's one present path
// (`psx_rt::present`, sdk/docs/PRESENT.md). `gpu_end_frame` hands the
// frame's ordering table to the presenter and returns at once: the presenter
// links the draw area, offset and clear of the buffer being drawn in front of
// it, kicks the walk as soon as the frame before is on screen and channel 2
// is idle, and psx-rt's VBlank handler flips to the finished frame at the
// first edge the cap allows (30 fps, the SDK's default). Every chain ends on
// [`psx_gpu::DRAW_DONE_NODE`], whose GP0(1Fh) the flip waits for.
//
// The CPU waits only when it is a whole frame ahead (the slot is still
// full), before rebuilding an arena whose walk has not finished, and before
// direct GP0/GP1 or GPU DMA access, which psx-rt's direct-access guard turns
// into a wait for everything in flight.

/// The world's draw mode (dithering on, texture page and window zero), the
/// first node after the presenter's preamble. A frame's primitives and the
/// screen tints change both, so each frame starts from this state.
#[repr(C, align(4))]
struct WorldState {
    tag: u32,
    draw_mode: u32,
    texture_window: u32,
}

const WORLD_MATERIAL: TextureMaterial = TextureMaterial::new(0, 0).with_dither(true);

const WORLD_STATE: WorldState = WorldState {
    tag: 0,
    draw_mode: WORLD_MATERIAL.draw_mode_word(),
    texture_window: WORLD_MATERIAL.texture_window_word(),
};

/// One per arena, since the walk of the other may still be reading its node.
static mut WORLD_STATES: [WorldState; 2] = [WORLD_STATE, WORLD_STATE];

#[cfg(feature = "hardware-performance")]
unsafe fn hardware_perf_reset() {
    unsafe {
        HARDWARE_PERF = HardwarePerformance::new();
        HARDWARE_PERF_LAST_PRESENT = 0;
        HARDWARE_PERF_HAS_PRESENT = false;
    }
}

#[cfg(feature = "hardware-performance")]
unsafe fn hardware_perf_record_gpu_wait(before: u32, after: u32) {
    unsafe {
        if !HARDWARE_PERF.active || !HARDWARE_PERF_HAS_PRESENT {
            return;
        }
        let elapsed = after.wrapping_sub(before);
        if elapsed == 0 {
            return;
        }
        HARDWARE_PERF.gpu_wait_events = HARDWARE_PERF.gpu_wait_events.wrapping_add(1);
        HARDWARE_PERF.gpu_wait_vblanks = HARDWARE_PERF.gpu_wait_vblanks.wrapping_add(elapsed);
        HARDWARE_PERF.gpu_wait_max_vblanks = HARDWARE_PERF.gpu_wait_max_vblanks.max(elapsed);
    }
}

#[cfg(feature = "hardware-performance")]
unsafe fn hardware_perf_record_present(now: u32) {
    unsafe {
        if !HARDWARE_PERF.active {
            return;
        }
        if !HARDWARE_PERF_HAS_PRESENT {
            HARDWARE_PERF_LAST_PRESENT = now;
            HARDWARE_PERF_HAS_PRESENT = true;
            return;
        }

        let elapsed = now.wrapping_sub(HARDWARE_PERF_LAST_PRESENT);
        HARDWARE_PERF_LAST_PRESENT = now;
        if elapsed == 0 {
            return;
        }
        #[cfg(feature = "emulator-telemetry")]
        {
            psx_telemetry::emit::counter(psx_telemetry::counter::SIM_TICKS, 1);
            psx_telemetry::emit::counter(psx_telemetry::counter::VISUAL_INTERVAL_VBLANKS, elapsed);
            let lateness = elapsed.saturating_sub(2);
            if lateness != 0 {
                psx_telemetry::emit::counter(psx_telemetry::counter::VISUAL_DEADLINE_MISSES, 1);
                psx_telemetry::emit::counter(
                    psx_telemetry::counter::VISUAL_SKIPPED_VBLANKS,
                    lateness,
                );
                psx_telemetry::emit::counter(
                    psx_telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS,
                    lateness,
                );
            }
        }
        HARDWARE_PERF.samples = HARDWARE_PERF.samples.wrapping_add(1);
        HARDWARE_PERF.total_vblanks = HARDWARE_PERF.total_vblanks.wrapping_add(elapsed);
        HARDWARE_PERF.max_vblanks = HARDWARE_PERF.max_vblanks.max(elapsed);
        match elapsed {
            1 => HARDWARE_PERF.one_vblank = HARDWARE_PERF.one_vblank.wrapping_add(1),
            2 => HARDWARE_PERF.two_vblank = HARDWARE_PERF.two_vblank.wrapping_add(1),
            _ => HARDWARE_PERF.three_plus_vblank = HARDWARE_PERF.three_plus_vblank.wrapping_add(1),
        }

        #[cfg(not(feature = "hardware-regression"))]
        if HARDWARE_PERF.samples >= HARDWARE_PERF_SAMPLE_FRAMES {
            HARDWARE_PERF.active = false;
            HARDWARE_PERF.complete = true;
            HARDWARE_PERF_HAS_PRESENT = false;
        }
    }
}

/// Initialise GPU state without enabling CPU interrupts.
///
/// The DualShock analog-mode exchange must run after this hardware reset but
/// before [`start_vblank_counter`]. Keeping the phases separate matches the
/// ordering already proven by VoXide on an original console.
#[optimize(size)]
pub fn gpu_init_before_interrupts() {
    // SAFETY: the token is a logic guard; nothing else owns the GPU DMA channel at boot. The driver
    // value is not kept: the rest of this module still draws through the tokenless calls.
    let mut gpu = psx_gpu::Gpu::new(
        unsafe { psx_io::periph::GpuDma::steal() },
        psx_gpu::display::DisplayConfig::new(
            psx_gpu::display::VideoMode::Ntsc,
            psx_gpu::display::Resolution::R320X240,
        ),
    );
    unsafe {
        BUILD_BUFFER = 0;
        DEFERRED_UPLOAD_COUNT = 0;
        BUFFERS = DoubleBuffer::with_stride(Resolution::R320X240, BACK_BUFFER_Y);
        gpu.set_draw_area((0, 0), (WIDTH - 1, HEIGHT - 1));
        gpu.set_draw_offset((0, 0));
        configure_world_material(&mut gpu);
        gpu.draw(&psx_gpu::prim::FillRect::new((0, 0), (512, 256), (0, 0, 0)));
        gpu.draw(&psx_gpu::prim::FillRect::new((0, 256), (512, 256), (0, 0, 0)));
        // The closing draw-done sequence fills a 16 x 8 block with black. The default
        // block (1008, 504) is menu picture data here; rows 496..511 left of x = 320 sit
        // below both framebuffers and are never written, so the sequence works there.
        psx_io::gpu::set_draw_done_scratch(0, 504);
        (&mut *addr_of_mut!(OTS).cast::<OrderingTable<OT_DEPTH>>()).clear();
        (&mut *addr_of_mut!(OTS).cast::<OrderingTable<OT_DEPTH>>().add(1)).clear();
        SCREEN_COMMAND_COUNT = 0;

        #[cfg(feature = "hardware-performance")]
        {
            hardware_perf_reset();
            #[cfg(feature = "hardware-regression")]
            {
                HARDWARE_PERF.active = false;
            }
        }
        #[cfg(feature = "emulator-telemetry")]
        {
            TELEMETRY_FRAME = 0;
            psx_telemetry::emit::frame_begin(TELEMETRY_FRAME);
            TELEMETRY_FRAME = TELEMETRY_FRAME.wrapping_add(1);
        }
    }
}

/// The shared PSoXide boot splash, drawn before the renderer owns the frame.
/// `buttons` reads the pad once per frame, for the press that skips it.
#[optimize(size)]
pub fn show_boot_splash(buttons: impl FnMut() -> psx_pad::ButtonState) {
    // SAFETY: nothing else touches the GPU until the splash returns.
    let mut dma = unsafe { psx_io::periph::GpuDma::steal() };
    // The splash draws into the renderer's own pair of buffers, so the
    // presenter's first frame follows where the display was left.
    psx_splash::show(
        psx_gpu::Gpu::from_dma_mut(&mut dma),
        unsafe { buffers() },
        "Built with PSoXide",
        psx_display::Brightness::DEFAULT,
        buttons,
    );
}

/// Enable the VBlank clock after the controller configuration transaction.
#[optimize(size)]
pub fn start_vblank_counter() {
    psx_rt::interrupts::install_vblank_counter();
    // Raises the draw-done flag once, so the first frame's edge finds it set,
    // and registers the direct-access guard. The GPU is idle: nothing has been
    // handed over yet.
    let clear = if cfg!(feature = "probe-clear-magenta") {
        // Diagnostic: a magenta clear makes every pixel no polygon covered (a
        // crack, a dropped oversize triangle) stand out from lit and dark
        // surfaces alike.
        (0xFF, 0x00, 0xFF)
    } else {
        (0x00, 0x00, 0x00)
    };
    // SAFETY: single-threaded; nothing reads the presenter before this returns.
    unsafe { (*addr_of_mut!(PRESENTER)).write(Presenter::new(Some(clear))) };
}

/// Configure Quake's 320x240 projection convention.
#[optimize(size)]
pub fn configure_quake_projection() {
    scene::set_screen_offset(160 << 16, 120 << 16);
    scene::set_projection_plane(160);
    scene::set_average_z_weights(0x155, 0x100);
}

/// Apply one render frame's underwater projection. The offsets are pixels;
/// the GTE registers consume signed 15.16 fixed point.
#[optimize(size)]
pub fn configure_underwater_projection(offset_x: i16, offset_y: i16, plane: u16) {
    scene::set_screen_offset(
        (160 + i32::from(offset_x)) << 16,
        (120 + i32::from(offset_y)) << 16,
    );
    scene::set_projection_plane(plane);
}

/// Camera transform retained for composing model-local alias transforms.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct QuakeViewTransform {
    pub rotation: Mat3I16,
    pub translation: Vec3I32,
}

/// Quake's Z-up to PlayStation view-space basis, including the retained 3x
/// world scale. View models use this basis directly so they remain fixed to
/// the camera instead of inheriting the player's world-facing angles.
pub const fn quake_coordinate_rotation() -> Mat3I16 {
    Mat3I16 {
        m: [[0, -0x3000, 0], [0, 0, -0x3000], [0x3000, 0, 0]],
    }
}

/// The camera's X, then Y, then Z rotation at the full 4096-per-turn angle
/// resolution `Player::view_angles` carries.
///
/// `Mat3I16::rotate_xyz` takes 256 steps per turn, so passing it `angle >> 4`
/// drew the view in 1.4 degree steps (five pixels at the screen centre): a
/// slow turn or look arrived as jumps, and the strafe roll, which never
/// exceeds two degrees, took only the values 0, +1.4 and (arithmetic shift of
/// a negative angle) -2.8. `rotate_x_q12` and `rotate_y_q12` interpolate the
/// same table, and agree with `rotate_x` and `rotate_y` exactly on every
/// multiple of 16.
pub fn quake_view_rotation(angles: [i16; 3]) -> Mat3I16 {
    let roll = angles[2] as u16;
    let (c, s) = (psx_math::cos_q12(roll) as i16, psx_math::sin_q12(roll) as i16);
    Mat3I16::rotate_x_q12(angles[0] as u16)
        .mul(&Mat3I16::rotate_y_q12(angles[1] as u16))
        .mul(&Mat3I16 {
            m: [[c, -s, 0], [s, c, 0], [0, 0, 0x1000]],
        })
}

/// Load the Quake view transform directly into the GTE.
pub fn load_quake_camera(origin_q12: [i32; 3], angles: [i16; 3]) -> QuakeViewTransform {
    let view = quake_view_rotation(angles);
    let coordinates = quake_coordinate_rotation();
    let rotation = scene::compose_rotation_scheduled(&view, &coordinates);
    scene::load_rotation(&rotation);
    scene::load_translation(Vec3I32::ZERO);
    let translation = scene::transform_vertex_scheduled(Vec3I16::new(
        (-origin_q12[0] >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        (-origin_q12[1] >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        (-origin_q12[2] >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
    ));
    scene::load_translation(translation);
    QuakeViewTransform {
        rotation,
        translation,
    }
}

/// Start building the next double-buffered frame.
/// Primitive classes for the depth-sort probe (PSoXide `--sort-log`).
pub mod sort_class {
    pub const WORLD: u32 = 1;
    pub const MODEL: u32 = 2;
    pub const VIEW: u32 = 3;
    pub const SPRITE: u32 = 4;
    pub const SKY: u32 = 7;
}

pub fn gpu_begin_frame() {
    unsafe {
        #[cfg(feature = "emulator-telemetry")]
        {
            psx_telemetry::emit::frame_begin(TELEMETRY_FRAME);
            TELEMETRY_FRAME = TELEMETRY_FRAME.wrapping_add(1);
        }
        BUILD_BUFFER ^= 1;
        psx_rt::present::wait_arena_free();
        build_ot().clear();
        // Every chain ends on the shared GP0(1Fh) node, which raises the
        // draw-done flag the VBlank handler flips on.
        build_ot().end_with_draw_done();
        SCREEN_COMMAND_COUNT = 0;
    }
}

/// Register equal-sized screen-space packets for ordered HUD/view-model draw.
///
/// # Safety
/// `packet_start` must identify `packet_count` live packets of `words + 1`
/// `u32`s through the end of [`gpu_end_frame`].
pub unsafe fn register_screen_packets(
    packet_start: *mut u32,
    packet_count: usize,
    words: u8,
) -> usize {
    if packet_start.is_null() || words > 15 {
        return 0;
    }
    let before = unsafe { SCREEN_COMMAND_COUNT };
    let packet_words = words as usize + 1;
    let mut packet = 0usize;
    while packet < packet_count && unsafe { SCREEN_COMMAND_COUNT } < MAX_SCREEN_COMMANDS {
        let packet_ptr = unsafe { packet_start.add(packet * packet_words) };
        let index = unsafe { SCREEN_COMMAND_COUNT } * 2;
        unsafe {
            SCREEN_COMMANDS[index] = packet_ptr as usize;
            SCREEN_COMMANDS[index + 1] = (words as usize) << 24;
            SCREEN_COMMAND_COUNT += 1;
        }
        packet += 1;
    }
    unsafe { SCREEN_COMMAND_COUNT }.saturating_sub(before)
}

#[cfg(feature = "visual-parity-regression")]
pub fn registered_screen_packet_count() -> usize {
    unsafe { SCREEN_COMMAND_COUNT }
}

/// Finish and asynchronously submit one staged classic-affine packet stream.
///
/// # Safety
/// `packet_start..packet_end` must be one valid PSoXide tagged packet stream
/// that remains live until the next call completes the pending submission.
pub unsafe fn gpu_end_frame(packet_start: *mut u32, packet_end: *mut u32) {
    if !packet_start.is_null() && !packet_end.is_null() {
        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::stage_begin(psx_telemetry::stage::OT_SUBMIT);
        unsafe {
            build_ot().resume_frame().add_tagged_packet_stream_unchecked(packet_start, packet_end);
        }

        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::stage_end(psx_telemetry::stage::OT_SUBMIT);
    }
    unsafe { submit_frame() }
}

/// Hand the built frame to the presenter and return without waiting for the
/// edge.
unsafe fn submit_frame() {
    #[cfg(feature = "hardware-performance")]
    let slot_wait_start = psx_rt::interrupts::vblank_count();
    if unsafe { DEFERRED_UPLOAD_COUNT } != 0 {
        // Image loads go through the GP0 FIFO, so the walk before this frame
        // must be off the GPU first: the first direct access waits for
        // everything in flight (the direct-access guard).
        unsafe { flush_deferred_vram_uploads() };
    }
    #[cfg(feature = "emulator-telemetry")]
    psx_telemetry::emit::counter(psx_telemetry::counter::VISUAL_FRAMES, 1);

    let ot = unsafe { build_ot() };
    unsafe {
        ot.resume_frame().add_packed_commands_reverse_unchecked(
            addr_of!(SCREEN_COMMANDS).cast::<usize>(),
            SCREEN_COMMAND_COUNT,
        );
    }
    let world = unsafe { &mut (*addr_of_mut!(WORLD_STATES))[BUILD_BUFFER] };
    world.tag = (2 << 24) | (ot.submit_head() as u32 & 0x00FF_FFFF);
    // SAFETY: the chain is this arena's world state, ordering table and
    // packets, every node at most 16 words and ending on the shared GP0(1Fh)
    // node; none of it is rebuilt before wait_arena_free says its walk has
    // ended.
    unsafe { presenter().submit(buffers(), addr_of!(world.tag), Part::Final) };
    #[cfg(feature = "hardware-performance")]
    unsafe {
        let now = psx_rt::interrupts::vblank_count();
        hardware_perf_record_gpu_wait(slot_wait_start, now);
        hardware_perf_record_present(now);
    }
}

/// Wait until the frame just submitted is on screen and nothing is in flight.
///
/// Normal gameplay pipelines one frame. Blocking map I/O has no later frame
/// to push the loading image through, so the loading path waits for it here.
pub fn gpu_present_pending_frame() {
    psx_rt::present::wait_idle();
}

#[cfg(feature = "hardware-performance")]
pub fn hardware_performance() -> HardwarePerformance {
    unsafe { HARDWARE_PERF }
}

#[cfg(feature = "hardware-performance")]
pub fn hardware_performance_pause() {
    unsafe {
        HARDWARE_PERF.active = false;
        HARDWARE_PERF_HAS_PRESENT = false;
    }
}

#[cfg(feature = "hardware-performance")]
pub fn hardware_performance_resume() {
    unsafe {
        if !HARDWARE_PERF.complete {
            HARDWARE_PERF.active = true;
            HARDWARE_PERF_HAS_PRESENT = false;
        }
    }
}

#[cfg(feature = "hardware-performance")]
pub fn hardware_performance_finish() {
    unsafe {
        HARDWARE_PERF.active = false;
        HARDWARE_PERF.complete = true;
        HARDWARE_PERF_HAS_PRESENT = false;
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VramUploadError {
    Empty,
    SizeMismatch,
}

/// One rectangle backed by a byte range in a shared upload buffer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VramUploadRange {
    pub rect: VramRect,
    pub start: usize,
    pub len: usize,
}

/// Upload one exact 16-bit VRAM rectangle after the prior frame is idle.
#[optimize(size)]
pub fn upload_vram(rect: VramRect, bytes: &[u8]) -> Result<(), VramUploadError> {
    if bytes.is_empty() {
        return Err(VramUploadError::Empty);
    }
    if bytes.len() != rect.w as usize * rect.h as usize * 2 {
        return Err(VramUploadError::SizeMismatch);
    }
    psx_rt::present::wait_idle();
    psx_vram::upload_bytes(rect, bytes);
    // SAFETY: as in `show_boot_splash`: nothing else touches the GPU while the upload completes.
    psx_gpu::Gpu::from_dma_mut(&mut unsafe { psx_io::periph::GpuDma::steal() }).wait_idle();
    Ok(())
}

// Four simultaneously visible liquid tiles plus the old/new weapon slots of
// one selection change. Calls append, allowing the immutable weapon blob and
// the static liquid-warp buffer to share one end-of-frame fence.
const MAX_DEFERRED_UPLOAD_RANGES: usize = 6;

const EMPTY_DEFERRED_RANGE: VramUploadRange = VramUploadRange {
    rect: VramRect::new(0, 0, 1, 1),
    start: 0,
    len: 2,
};

#[derive(Copy, Clone)]
struct DeferredVramUpload {
    rect: VramRect,
    source: *const u8,
    len: usize,
}

const EMPTY_DEFERRED_UPLOAD: DeferredVramUpload = DeferredVramUpload {
    rect: EMPTY_DEFERRED_RANGE.rect,
    source: core::ptr::null(),
    len: 0,
};

static mut DEFERRED_UPLOADS: [DeferredVramUpload; MAX_DEFERRED_UPLOAD_RANGES] =
    [EMPTY_DEFERRED_UPLOAD; MAX_DEFERRED_UPLOAD_RANGES];
static mut DEFERRED_UPLOAD_COUNT: usize = 0;

/// Stage several disjoint inactive-atlas uploads for the end of this frame.
///
/// Liquid animation double-buffers every tile, so the rewritten rectangles
/// are sampled by no in-flight frame. Performing the actual GP0 `A0` FIFO
/// writes inside [`gpu_end_frame`], after the previous frame's fence, means
/// the renderer never stalls mid-build waiting for GPU command readiness,
/// and the frame that references the new tiles cannot start rasterising
/// before its uploads have fully entered the FIFO.
///
/// # Safety
/// `bytes` must stay live and unmodified until the next [`gpu_end_frame`]
/// returns. Calls append: the renderer satisfies this with its static warp
/// buffer and the map-lifetime immutable weapon-icon blob.
pub unsafe fn defer_vram_ranges(
    bytes: &[u8],
    ranges: &[VramUploadRange],
) -> Result<(), VramUploadError> {
    if ranges.is_empty() {
        return Ok(());
    }
    let first = unsafe { DEFERRED_UPLOAD_COUNT };
    let Some(total) = first.checked_add(ranges.len()) else {
        return Err(VramUploadError::SizeMismatch);
    };
    if total > MAX_DEFERRED_UPLOAD_RANGES {
        return Err(VramUploadError::SizeMismatch);
    }
    for range in ranges {
        if range.len == 0
            || range.len != range.rect.w as usize * range.rect.h as usize * 2
            || range
                .start
                .checked_add(range.len)
                .filter(|&end| end <= bytes.len())
                .is_none()
        {
            return Err(VramUploadError::SizeMismatch);
        }
    }
    unsafe {
        for (index, range) in ranges.iter().enumerate() {
            DEFERRED_UPLOADS[first + index] = DeferredVramUpload {
                rect: range.rect,
                source: bytes.as_ptr().add(range.start),
                len: range.len,
            };
        }
        DEFERRED_UPLOAD_COUNT = total;
    }
    Ok(())
}

/// Perform the staged FIFO uploads while the GPU is provably idle.
///
/// Every range was validated by [`defer_vram_ranges`]; the raw writes here
/// cannot fail, which keeps "commit the double-buffer flip at staging time"
/// sound.
unsafe fn flush_deferred_vram_uploads() {
    let count = unsafe { DEFERRED_UPLOAD_COUNT };
    if count == 0 {
        return;
    }
    for upload in unsafe { &DEFERRED_UPLOADS[..count] } {
        let range_bytes = unsafe { core::slice::from_raw_parts(upload.source, upload.len) };
        if range_bytes.as_ptr().is_aligned() && range_bytes.len().is_multiple_of(4) {
            let words = unsafe {
                core::slice::from_raw_parts(
                    range_bytes.as_ptr().cast::<u32>(),
                    range_bytes.len() / 4,
                )
            };
            psx_vram::upload_words(upload.rect, words);
        } else {
            psx_vram::upload_bytes(upload.rect, range_bytes);
        }
    }
    unsafe {
        DEFERRED_UPLOAD_COUNT = 0;
    }
}

/// The pack's chunk table is one sector: this many entries is the pack's whole directory (the
/// build's pack has 17 chunks), so it is read from the disc once and every lookup after that is
/// a table scan in RAM.
const PACK_TABLE_CAP: usize = 17;

/// One directory entry, as much of a `PackEntry` as the reads below use.
#[derive(Copy, Clone)]
struct TableEntry {
    chunk_id: u32,
    sector_offset: u32,
    byte_size: u32,
}

impl TableEntry {
    const EMPTY: Self = Self { chunk_id: 0, sector_offset: 0, byte_size: 0 };
}

/// Read the directory sector (one session) into the table. `false` when the disc does not
/// answer or the directory is not a single sector of at most `PACK_TABLE_CAP` chunks; the
/// callers then look each chunk up through the SDK's scan.
#[optimize(size)]
unsafe fn read_pack_table() -> bool {
    let reader = unsafe { &mut *addr_of_mut!(READER) };
    let sector = unsafe { &mut *addr_of_mut!(SECTOR) };
    if !unsafe { begin_session(reader, WORLD_PACK_DEFAULT_LBA) } {
        reader.stop();
        return false;
    }
    let read = reader.read_sector(sector);
    reader.stop();
    if !read {
        return false;
    }
    // SAFETY: the sector buffer is `SECTOR_BYTES` of initialised words.
    let bytes = unsafe { core::slice::from_raw_parts(sector.as_ptr().cast::<u8>(), SECTOR_BYTES) };
    let Some(header) = parse_header(bytes) else {
        return false;
    };
    let count = header.chunk_count as usize;
    if header.header_sectors != 1 || count == 0 || count > PACK_TABLE_CAP {
        return false;
    }
    for index in 0..count {
        let Some(entry) = parse_entry(bytes, index) else {
            return false;
        };
        unsafe {
            PACK_TABLE[index] = TableEntry {
                chunk_id: entry.chunk_id,
                sector_offset: entry.sector_offset,
                byte_size: entry.byte_size,
            };
        }
    }
    unsafe { PACK_TABLE_LEN = count };
    true
}

unsafe fn entry_for(chunk_id: u32) -> Option<PackEntry> {
    if unsafe { PACK_TABLE_LEN } == 0 && !unsafe { STORAGE_STREAM_ACTIVE } {
        unsafe { read_pack_table() };
    }
    if unsafe { PACK_TABLE_LEN } != 0 {
        let table = unsafe { &(&*addr_of!(PACK_TABLE))[..PACK_TABLE_LEN] };
        let entry = table.iter().find(|entry| entry.chunk_id == chunk_id)?;
        return Some(PackEntry {
            chunk_id,
            sector_offset: entry.sector_offset,
            sector_count: entry.byte_size.div_ceil(SECTOR_BYTES as u32),
            byte_size: entry.byte_size,
            checksum: 0,
        });
    }
    // The directory did not fit the table: scan the disc for this chunk, as the SDK does.
    if unsafe { STORAGE_STREAM_ACTIVE } {
        return None;
    }
    let reader = unsafe { &mut *addr_of_mut!(READER) };
    let sector = unsafe { &mut *addr_of_mut!(SECTOR) };
    find_entry(reader, WORLD_PACK_DEFAULT_LBA, chunk_id, sector)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StorageError {
    MissingChunk,
    OutOfBounds,
    ReadFailed,
}

/// Last CD failure snapshot for the on-screen loading error.
pub fn storage_diag() -> u32 {
    unsafe { (&*addr_of!(READER)).diagnostics() }
}

#[optimize(size)]
pub fn chunk_size(chunk_id: u32) -> Result<u32, StorageError> {
    unsafe { entry_for(chunk_id).map(|entry| entry.byte_size) }.ok_or(StorageError::MissingChunk)
}

/// One forward-only read session over a world-pack chunk.
///
/// Keeping `ReadN` active is important for large, sequential PSB payloads:
/// starting and pausing the drive for every eight-row texture upload was the
/// dominant cost of the all-Rust loader. Only one stream may exist because the
/// SDK sector reader and its aligned sector buffer are platform singletons.
pub struct ChunkStream {
    entry: PackEntry,
    cursor: u32,
    sector_pos: usize,
    sector_loaded: bool,
    active: bool,
}

impl ChunkStream {
    #[optimize(size)]
    pub fn open_at(chunk_id: u32, offset: u32) -> Result<Self, StorageError> {
        let entry = unsafe { entry_for(chunk_id) }.ok_or(StorageError::MissingChunk)?;
        if offset >= entry.byte_size || unsafe { STORAGE_STREAM_ACTIVE } {
            return Err(if offset >= entry.byte_size {
                StorageError::OutOfBounds
            } else {
                StorageError::ReadFailed
            });
        }

        let first_sector = offset as usize / SECTOR_BYTES;
        let absolute_lba = WORLD_PACK_DEFAULT_LBA + entry.sector_offset + first_sector as u32;
        let reader = unsafe { &mut *addr_of_mut!(READER) };
        let started = unsafe { begin_session(reader, absolute_lba) };
        if !started {
            unsafe { reader.stop() };
            return Err(StorageError::ReadFailed);
        }
        unsafe { STORAGE_STREAM_ACTIVE = true };
        Ok(Self {
            entry,
            cursor: offset,
            sector_pos: offset as usize % SECTOR_BYTES,
            sector_loaded: false,
            active: true,
        })
    }

    /// Read at the current position or skip forward within the same `ReadN`
    /// session. Backward/random reads are rejected rather than silently
    /// restarting the drive.
    #[optimize(size)]
    pub fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), StorageError> {
        let count = u32::try_from(output.len()).map_err(|_| StorageError::OutOfBounds)?;
        if offset < self.cursor
            || offset > self.entry.byte_size
            || count > self.entry.byte_size - offset
        {
            return Err(StorageError::OutOfBounds);
        }
        self.skip_to(offset)?;
        self.copy_into(output)
    }

    #[optimize(size)]
    fn skip_to(&mut self, offset: u32) -> Result<(), StorageError> {
        while self.cursor < offset {
            self.ensure_sector()?;
            let available = SECTOR_BYTES - self.sector_pos;
            let skip = available.min((offset - self.cursor) as usize);
            self.sector_pos += skip;
            self.cursor += skip as u32;
            self.finish_sector_if_consumed();
        }
        Ok(())
    }

    #[optimize(size)]
    fn copy_into(&mut self, output: &mut [u8]) -> Result<(), StorageError> {
        let mut copied = 0usize;
        while copied < output.len() {
            self.ensure_sector()?;
            let take = (output.len() - copied).min(SECTOR_BYTES - self.sector_pos);
            let sector =
                unsafe { core::slice::from_raw_parts(addr_of!(SECTOR).cast::<u8>(), SECTOR_BYTES) };
            output[copied..copied + take]
                .copy_from_slice(&sector[self.sector_pos..self.sector_pos + take]);
            copied += take;
            self.sector_pos += take;
            self.cursor += take as u32;
            self.finish_sector_if_consumed();
        }
        if self.cursor == self.entry.byte_size {
            self.stop();
        }
        Ok(())
    }

    #[optimize(size)]
    fn ensure_sector(&mut self) -> Result<(), StorageError> {
        if self.sector_loaded {
            return Ok(());
        }
        let read = unsafe { (&mut *addr_of_mut!(READER)).read_sector(&mut *addr_of_mut!(SECTOR)) };
        if !read {
            self.stop();
            return Err(StorageError::ReadFailed);
        }
        self.sector_loaded = true;
        Ok(())
    }

    #[optimize(size)]
    fn finish_sector_if_consumed(&mut self) {
        if self.sector_pos == SECTOR_BYTES {
            self.sector_pos = 0;
            self.sector_loaded = false;
        }
    }

    #[optimize(size)]
    fn stop(&mut self) {
        if self.active {
            unsafe {
                (&mut *addr_of_mut!(READER)).stop();
                STORAGE_STREAM_ACTIVE = false;
            }
            self.active = false;
        }
    }
}

impl Drop for ChunkStream {
    #[optimize(size)]
    fn drop(&mut self) {
        self.stop();
    }
}


/// GetStat bit 5: the drive is reading.
const DRIVE_STAT_READING: u8 = 0x20;
/// Attempts at one read session before the caller sees a failure.
const SESSION_ATTEMPTS: u32 = 3;
/// GetStat polls (about a quarter of a second on silicon) that wait for a
/// Pause to finish.
const SETTLE_POLLS: u32 = 400;

/// Wait until the drive reports it is no longer reading. The reader's own
/// `stop` gives up on the Pause completion after 16,384 polls, but on silicon
/// that completion arrives 1.2 to 3.9 million cycles after the acknowledge
/// (hardware-tests v2.7), so the next command can find the drive still
/// stopping. The emulator completes a Pause in about 26,000 cycles and never
/// shows it. Only the retry path pays for this wait.
#[optimize(size)]
#[inline(never)]
unsafe fn settle_drive(reader: &mut SectorReader) {
    let cd = reader.cd_mut();
    for _ in 0..SETTLE_POLLS {
        let Some(saved) = cd.dispatch_command(0x01, &[], 16_384) else {
            continue;
        };
        let mut spins = 0u32;
        while cd.irq_flag_value() != 3 && spins < 16_384 {
            spins += 1;
        }
        let answered = spins < 16_384;
        let status = cd.read_response_byte();
        cd.restore_irq_output(saved);
        if answered && status & DRIVE_STAT_READING == 0 {
            return;
        }
    }
}

/// Open a read session at `lba`, retrying after the drive has settled.
#[optimize(size)]
#[inline(never)]
unsafe fn begin_session(reader: &mut SectorReader, lba: u32) -> bool {
    for _ in 0..SESSION_ATTEMPTS {
        if reader.prepare() && reader.start_read(lba) {
            return true;
        }
        reader.stop();
        settle_drive(reader);
    }
    false
}

#[optimize(size)]
unsafe fn read_storage_burst(entry: PackEntry, offset: u32, output: &mut [u8]) -> bool {
    for _ in 0..SESSION_ATTEMPTS {
        if unsafe { read_storage_burst_once(entry, offset, output) } {
            return true;
        }
        settle_drive(unsafe { &mut *addr_of_mut!(READER) });
    }
    false
}

unsafe fn read_storage_burst_once(entry: PackEntry, offset: u32, output: &mut [u8]) -> bool {
    let reader = unsafe { &mut *addr_of_mut!(READER) };
    let sector = unsafe { &mut *addr_of_mut!(SECTOR) };
    let first_sector = offset as usize / SECTOR_BYTES;
    let mut skip = offset as usize % SECTOR_BYTES;
    let absolute_lba = WORLD_PACK_DEFAULT_LBA + entry.sector_offset + first_sector as u32;
    if !begin_session(reader, absolute_lba) {
        reader.stop();
        return false;
    }
    let mut copied = 0usize;
    while copied < output.len() {
        if !reader.read_sector(sector) {
            reader.stop();
            return false;
        }
        let take = (output.len() - copied).min(SECTOR_BYTES - skip);
        let sector_bytes =
            unsafe { core::slice::from_raw_parts(sector.as_ptr().cast::<u8>(), SECTOR_BYTES) };
        output[copied..copied + take].copy_from_slice(&sector_bytes[skip..skip + take]);
        copied += take;
        skip = 0;
    }
    reader.stop();
    true
}

#[optimize(size)]
unsafe fn fill_storage_cache(chunk_id: u32, entry: PackEntry, offset: u32) -> bool {
    for _ in 0..SESSION_ATTEMPTS {
        if unsafe { fill_storage_cache_once(chunk_id, entry, offset) } {
            return true;
        }
        settle_drive(unsafe { &mut *addr_of_mut!(READER) });
    }
    false
}

unsafe fn fill_storage_cache_once(chunk_id: u32, entry: PackEntry, offset: u32) -> bool {
    let aligned = offset / SECTOR_BYTES as u32 * SECTOR_BYTES as u32;
    let remaining = (entry.byte_size - aligned) as usize;
    let sectors = remaining.div_ceil(SECTOR_BYTES).min(STORAGE_CACHE_SECTORS);
    let reader = unsafe { &mut *addr_of_mut!(READER) };
    let cache = unsafe { &mut *addr_of_mut!(STORAGE_CACHE) };
    let absolute_lba = WORLD_PACK_DEFAULT_LBA + entry.sector_offset + aligned / SECTOR_BYTES as u32;
    if !begin_session(reader, absolute_lba) {
        reader.stop();
        return false;
    }
    for index in 0..sectors {
        let slot = unsafe {
            &mut *(cache.as_mut_ptr().add(index * SECTOR_WORDS) as *mut [u32; SECTOR_WORDS])
        };
        if !reader.read_sector(slot) {
            reader.stop();
            return false;
        }
    }
    reader.stop();
    unsafe {
        STORAGE_CACHE_CHUNK = chunk_id;
        STORAGE_CACHE_OFFSET = aligned;
        STORAGE_CACHE_LEN = remaining.min(STORAGE_CACHE_BYTES);
    }
    true
}

/// Read an exact byte range from a cooked world-pack chunk.
#[optimize(size)]
pub fn read_chunk_exact(chunk_id: u32, offset: u32, output: &mut [u8]) -> Result<(), StorageError> {
    if unsafe { STORAGE_STREAM_ACTIVE } {
        return Err(StorageError::ReadFailed);
    }
    let entry = unsafe { entry_for(chunk_id) }.ok_or(StorageError::MissingChunk)?;
    let count = u32::try_from(output.len()).map_err(|_| StorageError::OutOfBounds)?;
    if offset > entry.byte_size || count > entry.byte_size - offset {
        return Err(StorageError::OutOfBounds);
    }
    if output.is_empty() {
        return Ok(());
    }

    let cache_end = unsafe { STORAGE_CACHE_OFFSET.saturating_add(STORAGE_CACHE_LEN as u32) };
    let request_end = offset + count;
    if unsafe {
        STORAGE_CACHE_CHUNK == chunk_id
            && offset >= STORAGE_CACHE_OFFSET
            && request_end <= cache_end
    } {
        let source = unsafe {
            core::slice::from_raw_parts(
                addr_of!(STORAGE_CACHE)
                    .cast::<u8>()
                    .add((offset - STORAGE_CACHE_OFFSET) as usize),
                output.len(),
            )
        };
        output.copy_from_slice(source);
        return Ok(());
    }

    if output.len() <= STORAGE_CACHE_BYTES / 2 {
        if !unsafe { fill_storage_cache(chunk_id, entry, offset) } {
            return Err(StorageError::ReadFailed);
        }
        let source = unsafe {
            core::slice::from_raw_parts(
                addr_of!(STORAGE_CACHE)
                    .cast::<u8>()
                    .add((offset - STORAGE_CACHE_OFFSET) as usize),
                output.len(),
            )
        };
        output.copy_from_slice(source);
        return Ok(());
    }

    if unsafe { read_storage_burst(entry, offset, output) } {
        Ok(())
    } else {
        Err(StorageError::ReadFailed)
    }
}

/// Let the last Pause of a load finish before the game runs: a Pause that is
/// still completing when the drive is next commanded (the music, a save) is
/// the silicon behaviour the emulator never shows.
#[optimize(size)]
#[inline(never)]
pub fn settle_after_load() {
    // SAFETY: called at a level boundary with no stream open.
    unsafe { settle_drive(&mut *addr_of_mut!(READER)) };
}
