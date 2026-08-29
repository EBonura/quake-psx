//! PSoXide-backed PlayStation services for the Rust Quake runtime.
//!
//! This module deliberately exposes Rust APIs only. The retired native game
//! implementation remains available through Git history, never through the
//! shipping build or a compatibility ABI.

#[cfg(feature = "renderer-window-range-coalescing")]
use core::ptr;
use core::ptr::{addr_of, addr_of_mut};

use psx_gpu::framebuf::FrameBuffer;
use psx_gpu::ot::OrderingTable;
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::scene;
use psx_pack::cd::{find_entry, SectorReader, SECTOR_WORDS, WORLD_PACK_DEFAULT_LBA};
use psx_pack::{PackEntry, SECTOR_BYTES};
use psx_vram::VramRect;

const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;
const BACK_BUFFER_Y: u16 = 256;
#[cfg(not(feature = "renderer-compact-ot-256"))]
const OT_DEPTH: usize = 2048;
#[cfg(feature = "renderer-compact-ot-256")]
const OT_DEPTH: usize = 256;
// Menu and HUD glyphs share this late-command list. On PS1 the metadata is
// three KiB and packet storage remains double-buffered in the renderer.
const MAX_SCREEN_COMMANDS: usize = 384;
#[cfg(feature = "renderer-window-range-coalescing")]
const MAX_WINDOW_PACKET_RANGES: usize = 128;
#[cfg(not(feature = "renderer-streamed-sections"))]
const ASSET_COUNT: usize = 12;
#[cfg(feature = "renderer-streamed-sections")]
const ASSET_COUNT: usize = 21;
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
static mut FRAME_BUFFER: FrameBuffer = FrameBuffer::new_strided(WIDTH, HEIGHT, BACK_BUFFER_Y);
static mut SCREEN_COMMANDS: [usize; MAX_SCREEN_COMMANDS * 2] = [0; MAX_SCREEN_COMMANDS * 2];
static mut SCREEN_COMMAND_COUNT: usize = 0;
#[cfg(feature = "renderer-window-range-coalescing")]
static mut WINDOW_PACKET_RANGES: [usize; MAX_WINDOW_PACKET_RANGES * 2] =
    [0; MAX_WINDOW_PACKET_RANGES * 2];
#[cfg(feature = "renderer-window-range-coalescing")]
static mut WINDOW_PACKET_RANGE_COUNT: usize = 0;
static mut BUILD_BUFFER: usize = 0;
static mut GPU_SUBMISSION_PENDING: bool = false;

static mut READER: SectorReader = SectorReader::new();
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
static mut ASSET_CACHE: [Option<PackEntry>; ASSET_COUNT] = [None; ASSET_COUNT];
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
fn configure_world_material() {
    psx_gpu::material::TextureMaterial::new(0, 0)
        .with_dither(true)
        .apply_draw_mode();
}

unsafe fn build_ot() -> &'static mut OrderingTable<OT_DEPTH> {
    unsafe {
        &mut *addr_of_mut!(OTS)
            .cast::<OrderingTable<OT_DEPTH>>()
            .add(BUILD_BUFFER)
    }
}

unsafe fn framebuffer() -> &'static mut FrameBuffer {
    unsafe { &mut *addr_of_mut!(FRAME_BUFFER) }
}

unsafe fn wait_for_pending_submission() {
    if unsafe { GPU_SUBMISSION_PENDING } {
        psx_gpu::submit_linked_list_wait();
        psx_gpu::draw_sync();
    }
}

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
    psx_gpu::init(psx_gpu::VideoMode::Ntsc, psx_gpu::Resolution::R320X240);
    unsafe {
        BUILD_BUFFER = 0;
        GPU_SUBMISSION_PENDING = false;
        DEFERRED_UPLOAD_COUNT = 0;
        FRAME_BUFFER = FrameBuffer::new_strided(WIDTH, HEIGHT, BACK_BUFFER_Y);
        psx_gpu::set_draw_area(0, 0, WIDTH - 1, HEIGHT - 1);
        psx_gpu::set_draw_offset(0, 0);
        configure_world_material();
        psx_gpu::fill_rect(0, 0, 512, 256, 0, 0, 0);
        psx_gpu::fill_rect(0, 256, 512, 256, 0, 0, 0);
        (&mut *addr_of_mut!(OTS).cast::<OrderingTable<OT_DEPTH>>()).clear();
        (&mut *addr_of_mut!(OTS).cast::<OrderingTable<OT_DEPTH>>().add(1)).clear();
        SCREEN_COMMAND_COUNT = 0;
        #[cfg(feature = "renderer-window-range-coalescing")]
        {
            WINDOW_PACKET_RANGE_COUNT = 0;
        }
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

/// The display's frame buffer, for the boot intro that runs before the
/// renderer owns the frame. Immediate-mode drawing only; nothing else may be
/// touching the GPU while the caller holds this.
#[optimize(size)]
pub fn boot_framebuffer() -> &'static mut FrameBuffer {
    unsafe { framebuffer() }
}

/// Enable the VBlank clock after the controller configuration transaction.
#[optimize(size)]
pub fn start_vblank_counter() {
    psx_rt::interrupts::install_vblank_counter();
}

/// Configure Quake's 320x240 projection convention.
#[optimize(size)]
pub fn configure_quake_projection() {
    scene::set_screen_offset(160 << 16, 120 << 16);
    scene::set_projection_plane(160);
    scene::set_avsz_weights(0x155, 0x100);
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

/// Load the Quake view transform directly into the GTE.
pub fn load_quake_camera(origin_q12: [i32; 3], angles: [i16; 3]) -> QuakeViewTransform {
    let view = Mat3I16::rotate_xyz(
        (angles[0] as u16) >> 4,
        (angles[1] as u16) >> 4,
        (angles[2] as u16) >> 4,
    );
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
pub fn gpu_begin_frame() {
    unsafe {
        #[cfg(feature = "emulator-telemetry")]
        {
            psx_telemetry::emit::frame_begin(TELEMETRY_FRAME);
            TELEMETRY_FRAME = TELEMETRY_FRAME.wrapping_add(1);
        }
        BUILD_BUFFER ^= 1;
        build_ot().clear();
        SCREEN_COMMAND_COUNT = 0;
        #[cfg(feature = "renderer-window-range-coalescing")]
        {
            WINDOW_PACKET_RANGE_COUNT = 0;
        }
    }
}

/// Register one contiguous sequence of scoped-window packets for a narrow
/// post-link pass. Overflow only leaves redundant E2 commands in place.
#[cfg(feature = "renderer-window-range-coalescing")]
#[inline(always)]
pub unsafe fn register_world_window_packet_range(first: *mut u32, end: *mut u32) {
    if first >= end || unsafe { WINDOW_PACKET_RANGE_COUNT } == MAX_WINDOW_PACKET_RANGES {
        return;
    }
    let index = unsafe { WINDOW_PACKET_RANGE_COUNT } * 2;
    unsafe {
        WINDOW_PACKET_RANGES[index] = first as usize;
        WINDOW_PACKET_RANGES[index + 1] = end as usize;
        WINDOW_PACKET_RANGE_COUNT += 1;
    }
}

#[cfg(feature = "renderer-window-range-coalescing")]
#[inline(never)]
unsafe fn coalesce_registered_world_windows(world_first: *mut u32, world_end: *mut u32) {
    const ADDRESS_MASK: u32 = 0x00ff_ffff;
    const E2_MASK: u32 = 0xff00_0000;
    const E2: u32 = 0xe200_0000;
    let address_high = world_first as usize & !(ADDRESS_MASK as usize);
    let mut range_index = 0usize;
    while range_index < unsafe { WINDOW_PACKET_RANGE_COUNT } {
        let range = range_index * 2;
        let mut packet = unsafe { WINDOW_PACKET_RANGES[range] as *mut u32 };
        let end = unsafe { WINDOW_PACKET_RANGES[range + 1] as *mut u32 };
        while packet < end {
            let tag = unsafe { ptr::read(packet) };
            let words = (tag >> 24) as usize;
            let physical_next = unsafe { packet.add(words + 1) };
            let selector = unsafe { ptr::read(packet.add(1)) };
            let old = (address_high | (tag & ADDRESS_MASK) as usize) as *mut u32;
            if selector & E2_MASK == E2
                && selector != E2
                && old >= world_first
                && old < packet
                && old < world_end
                && unsafe { ptr::read(old.add(1)) } == selector
            {
                let old_tag = unsafe { ptr::read(old) };
                debug_assert!(old_tag >> 24 > 1);
                unsafe {
                    ptr::write(old.add(1), old_tag.wrapping_sub(1 << 24));
                    ptr::write(
                        packet,
                        ((tag.wrapping_sub(1 << 24)) & !ADDRESS_MASK)
                            | (((old as u32).wrapping_add(4)) & ADDRESS_MASK),
                    );
                }
            }
            packet = physical_next;
        }
        range_index += 1;
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

/// Link a completed prefix of the current world packet stream immediately.
///
/// Persistent subdivision packets can then be inserted at their exact source
/// position without copying their invariant payload back into that stream.
///
/// # Safety
///
/// `packet_start..packet_end` must be one live writable staged packet stream
/// owned by the current build buffer.
#[cfg(any(
    feature = "renderer-subdivision-cache",
    feature = "renderer-owned-sections"
))]
#[inline(always)]
pub unsafe fn gpu_insert_world_stream(packet_start: *mut u32, packet_end: *mut u32) {
    if packet_start < packet_end {
        unsafe {
            #[cfg(not(feature = "renderer-compact-ot-256"))]
            build_ot().insert_tagged_packet_stream_unchecked(packet_start, packet_end);
            #[cfg(feature = "renderer-compact-ot-256")]
            build_ot().insert_tagged_packet_stream_shifted_unchecked::<3>(packet_start, packet_end);
        }
        #[cfg(feature = "renderer-window-range-coalescing")]
        unsafe {
            coalesce_registered_world_windows(packet_start, packet_end);
        }
    }
}

/// Insert one fixed resident packet into the current world ordering table.
///
/// # Safety
///
/// `packet` must remain live and writable through GPU completion, `otz` must
/// be below `OT_DEPTH`, and `words` must match the packet payload.
#[cfg(any(
    feature = "renderer-subdivision-cache",
    feature = "renderer-owned-sections"
))]
#[inline(always)]
pub unsafe fn gpu_insert_resident_world_packet(packet: *mut u32, otz: u16, words: u8) {
    #[cfg(not(feature = "renderer-compact-ot-256"))]
    let slot = otz as usize;
    #[cfg(feature = "renderer-compact-ot-256")]
    let slot = usize::from(otz) >> 3;
    debug_assert!(slot < OT_DEPTH);
    unsafe {
        build_ot().insert_unchecked_tag_high(slot, packet, u32::from(words) << 24);
    }
}

/// Link one contiguous resident subdivision root whose tags contain staged
/// OT slots. This shares the SDK's compact MIPS stream linker with ordinary
/// output while leaving the fixed packet block in its destination pool.
///
/// # Safety
/// `packet_start..packet_end` must be a live writable tagged packet stream in
/// the active display pool and remain owned until GPU completion.
#[cfg(any(
    feature = "renderer-subdivision-cache",
    feature = "renderer-owned-sections"
))]
#[inline(always)]
pub unsafe fn gpu_insert_resident_world_stream(packet_start: *mut u32, packet_end: *mut u32) {
    if packet_start < packet_end {
        unsafe {
            #[cfg(not(feature = "renderer-compact-ot-256"))]
            build_ot().insert_tagged_packet_stream_unchecked(packet_start, packet_end);
            #[cfg(feature = "renderer-compact-ot-256")]
            build_ot().insert_tagged_packet_stream_shifted_unchecked::<3>(packet_start, packet_end);
        }
    }
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
            #[cfg(not(feature = "renderer-compact-ot-256"))]
            build_ot().insert_tagged_packet_stream_unchecked(packet_start, packet_end);
            #[cfg(feature = "renderer-compact-ot-256")]
            build_ot().insert_tagged_packet_stream_shifted_unchecked::<3>(packet_start, packet_end);
        }
        #[cfg(feature = "renderer-window-range-coalescing")]
        unsafe {
            // Only the explicitly registered scoped-window packet ranges are
            // inspected. The ordinary tagged-stream linker above therefore
            // remains its compact, proven implementation for every packet.
            coalesce_registered_world_windows(packet_start, packet_end);
        }
        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::stage_end(psx_telemetry::stage::OT_SUBMIT);
    }
    #[cfg(feature = "hardware-performance")]
    let gpu_wait_start = psx_rt::interrupts::vblank_count();
    if unsafe { GPU_SUBMISSION_PENDING } {
        unsafe { wait_for_pending_submission() };
    }
    #[cfg(feature = "hardware-performance")]
    unsafe {
        hardware_perf_record_gpu_wait(gpu_wait_start, psx_rt::interrupts::vblank_count());
    }
    psx_rt::interrupts::wait_vblank();
    if unsafe { GPU_SUBMISSION_PENDING } {
        unsafe { framebuffer().swap() };
    }

    #[cfg(feature = "hardware-performance")]
    unsafe {
        hardware_perf_record_present(psx_rt::interrupts::vblank_count());
    }
    // Flush after the presentation edge, not before it: the GPU is idle here
    // and the next vblank is a full frame away, so the FIFO transfer can
    // never push a frame past the edge it was about to catch.
    unsafe { flush_deferred_vram_uploads() };
    #[cfg(feature = "emulator-telemetry")]
    psx_telemetry::emit::counter(psx_telemetry::counter::VISUAL_FRAMES, 1);

    configure_world_material();
    unsafe { framebuffer().clear(0, 0, 0) };

    let ot = unsafe { build_ot() };
    unsafe {
        ot.insert_packed_commands_reverse_unchecked(
            addr_of!(SCREEN_COMMANDS).cast::<usize>(),
            SCREEN_COMMAND_COUNT,
        );
    }
    #[cfg(feature = "renderer-window-run-coalescing")]
    unsafe {
        // This pass runs only after both world and screen packets have their
        // final OT links. It removes the interior E2 reset/selector pair from
        // same-window runs while retaining the exact entry/exit GPU state.
        let _ = ot.coalesce_scoped_texture_windows();
    }
    ot.submit_async();
    unsafe {
        GPU_SUBMISSION_PENDING = true;
    }
}

/// Wait for the just-submitted frame and expose it before returning.
///
/// Normal gameplay intentionally pipelines one frame. Blocking map I/O has
/// no later frame with which to perform that swap, so the loading path uses
/// this explicit fence and leaves the completed loading image on screen.
pub fn gpu_present_pending_frame() {
    unsafe {
        if !GPU_SUBMISSION_PENDING {
            return;
        }
        wait_for_pending_submission();
        psx_rt::interrupts::wait_vblank();
        framebuffer().swap();
        GPU_SUBMISSION_PENDING = false;
    }
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
    unsafe { wait_for_pending_submission() };
    psx_vram::upload_bytes(rect, bytes);
    psx_gpu::draw_sync();
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

#[optimize(size)]
fn cache_index(chunk_id: u32) -> Option<usize> {
    match chunk_id {
        1 => Some(0),
        2 => Some(1),
        3 => Some(2),
        100 => Some(3),
        101..=108 => Some((chunk_id - 97) as usize),
        #[cfg(feature = "renderer-streamed-sections")]
        200..=208 => Some((chunk_id - 188) as usize),
        _ => None,
    }
}

unsafe fn entry_for(chunk_id: u32) -> Option<PackEntry> {
    let index = cache_index(chunk_id)?;
    if let Some(entry) = unsafe { ASSET_CACHE[index] } {
        return Some(entry);
    }
    let reader = unsafe { &mut *addr_of_mut!(READER) };
    let sector = unsafe { &mut *addr_of_mut!(SECTOR) };
    let entry = find_entry(reader, WORLD_PACK_DEFAULT_LBA, chunk_id, sector)?;
    unsafe { ASSET_CACHE[index] = Some(entry) };
    Some(entry)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StorageError {
    MissingChunk,
    OutOfBounds,
    ReadFailed,
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
        let started = unsafe { reader.prepare() && reader.start_read(absolute_lba) };
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

unsafe fn read_storage_burst(entry: PackEntry, offset: u32, output: &mut [u8]) -> bool {
    let reader = unsafe { &mut *addr_of_mut!(READER) };
    let sector = unsafe { &mut *addr_of_mut!(SECTOR) };
    let first_sector = offset as usize / SECTOR_BYTES;
    let mut skip = offset as usize % SECTOR_BYTES;
    let absolute_lba = WORLD_PACK_DEFAULT_LBA + entry.sector_offset + first_sector as u32;
    if !reader.prepare() || !reader.start_read(absolute_lba) {
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

unsafe fn fill_storage_cache(chunk_id: u32, entry: PackEntry, offset: u32) -> bool {
    let aligned = offset / SECTOR_BYTES as u32 * SECTOR_BYTES as u32;
    let remaining = (entry.byte_size - aligned) as usize;
    let sectors = remaining.div_ceil(SECTOR_BYTES).min(STORAGE_CACHE_SECTORS);
    let reader = unsafe { &mut *addr_of_mut!(READER) };
    let cache = unsafe { &mut *addr_of_mut!(STORAGE_CACHE) };
    let absolute_lba = WORLD_PACK_DEFAULT_LBA + entry.sector_offset + aligned / SECTOR_BYTES as u32;
    if !reader.prepare() || !reader.start_read(absolute_lba) {
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
