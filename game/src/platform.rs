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

const OT_DEPTH: usize = 2048;

// Menu and HUD glyphs share this late-command list. On PS1 the metadata is
// three KiB and packet storage remains double-buffered in the renderer.
const MAX_SCREEN_COMMANDS: usize = 384;

const ASSET_COUNT: usize = 12;
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
        #[cfg(feature = "present-queue")]
        present_queue::wait_slot_empty();
        psx_gpu::submit_linked_list_wait();
        psx_gpu::draw_sync();
    }
}

#[cfg(all(feature = "present-queue", feature = "hardware-performance"))]
compile_error!("the present queue does not record hardware-performance cadence");

/// Non-blocking present (`present-queue`).
///
/// The blocking path in [`gpu_end_frame`] serialises "previous raster done,
/// vblank edge, flip, kick" on the CPU, so the CPU idles until the edge even
/// when the next frame could already be building. Here the finished frame's
/// chain goes into a one-entry slot and the CPU returns at once; the VBlank
/// handler below applies the flip to the previous frame and kicks the queued
/// chain on the first edge where the previous chain's closing GP0(1Fh) has
/// run (GPUSTAT bit 24) and DMA channel 2 is idle, so a flip lands only on a
/// blank edge and only once that frame is fully drawn. This is psx-rt's
/// present protocol (`psx_gpu::draw_done`): every queued chain ends on
/// [`psx_gpu::DRAW_DONE_NODE`], the handler acknowledges the flag with
/// GP1(02h) after the flip and before the kick, and start-up raises it once
/// through the port so the first frame's edge finds it set. GPUSTAT bit 28 is
/// not a drawing-complete test: on silicon it rises when the walk has pushed
/// its last packet, about one large primitive before the drawing ends
/// (hardware-tests v1.24 cases 219-226).
///
/// The new back buffer's draw area, offset, draw mode and clear travel as the
/// chain's first packets, so the handler writes only GP1(05h), GP1(02h) and
/// the channel 2 kick. The CPU waits only when it is a whole frame ahead (the
/// slot is still full), before rebuilding an arena whose walk has not
/// finished, and before immediate GP0 image loads, which need the previous
/// frame off the GPU. The main thread touches GP0/GP1/DMA only while the slot
/// is empty, and never sends GP0(1Fh) through the port while channel 2 walks.
///
/// Both waits are bounded: a frame whose chain wedges or loses its GP0(1Fh)
/// would otherwise hold the slot forever. After [`STALL_VBLANKS`] edges the
/// CPU stops the walk, as `psx_gpu::submit_linked_list_wait` does, and raises
/// the flag itself (counted in `QUAKE_PRESENT_RECOVERIES`).
///
/// psx-rt's handler, which this one jumps into, resumes after a GTE command
/// the IRQ landed on (psx-rt 8055e87f6), so an edge taken on RTPS does not
/// run it twice. A GTE command in a branch delay slot is still exposed;
/// `hazard_scan.py` reports any.
#[cfg(any(feature = "present-queue", feature = "irq-epc-probe"))]
pub(crate) mod present_queue {
    use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

    /// Chain head the handler kicks at the next ready edge; 0 = empty. The
    /// handler owns the slot (and the GPU) while this is non-zero.
    #[no_mangle]
    pub static mut QUAKE_PRESENT_HEAD: u32 = 0;
    /// GP1(05h) word applied just before the kick; 0 = no flip.
    #[no_mangle]
    pub static mut QUAKE_PRESENT_DISPLAY: u32 = 0;
    /// Edges on which a queued frame found the previous one undrawn or
    /// channel 2 busy.
    #[no_mangle]
    pub static mut QUAKE_PRESENT_SKIPPED: u32 = 0;
    /// Frames the handler has kicked.
    #[no_mangle]
    pub static mut QUAKE_PRESENT_KICKS: u32 = 0;
    /// Times the CPU gave up on a stalled chain (see [`STALL_VBLANKS`]).
    #[no_mangle]
    pub static mut QUAKE_PRESENT_RECOVERIES: u32 = 0;

    #[cfg(feature = "irq-epc-probe")]
    #[no_mangle]
    pub static mut QUAKE_IRQ_EPC: [u32; 1024] = [0; 1024];
    #[cfg(feature = "irq-epc-probe")]
    #[no_mangle]
    pub static mut QUAKE_IRQ_EPC_COUNT: u32 = 0;

    const PRESENT: u32 = cfg!(feature = "present-queue") as u32;
    const PROBE: u32 = cfg!(feature = "irq-epc-probe") as u32;

    /// Edges a queued frame may wait before the CPU treats the chain ahead
    /// of it as stalled. A Quake frame's GPU work is well under one field.
    pub const STALL_VBLANKS: u32 = 8;

    // Runs ahead of psx-rt's handler, which still counts the VBlank, applies
    // its own GP1 queue, acknowledges the IRQ and returns. Only $k0/$k1.
    #[cfg(target_arch = "mips")]
    core::arch::global_asm!(
        r#"
    .set noreorder
    .section .text.quake_exception_handler
    .globl quake_exception_handler
quake_exception_handler:
    lui   $26, 0x1f80
    lw    $27, 0x1070($26)
    lw    $26, 0x1074($26)
    nop
    and   $27, $27, $26
    andi  $27, $27, 0x0001
    beqz  $27, 9f
    nop
.if {probe}
    lui   $26, %hi(QUAKE_IRQ_EPC_COUNT)
    lw    $27, %lo(QUAKE_IRQ_EPC_COUNT)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(QUAKE_IRQ_EPC_COUNT)($26)
    addiu $27, $27, -1
    andi  $27, $27, 0x3ff
    sll   $27, $27, 2
    lui   $26, %hi(QUAKE_IRQ_EPC)
    addiu $26, $26, %lo(QUAKE_IRQ_EPC)
    addu  $26, $26, $27
    mfc0  $27, $14
    nop
    sw    $27, 0($26)
.endif
.if {present}
    lui   $26, %hi(QUAKE_PRESENT_HEAD)
    lw    $27, %lo(QUAKE_PRESENT_HEAD)($26)
    nop
    beqz  $27, 9f
    nop
    # The previous chain's closing GP0(1Fh) has run: GPUSTAT bit 24.
    lui   $26, 0x1f80
    lw    $27, 0x1814($26)
    nop
    srl   $27, $27, 24
    andi  $27, $27, 1
    beqz  $27, 8f
    nop
    lw    $27, 0x10a8($26)
    nop
    srl   $27, $27, 24
    andi  $27, $27, 1
    bnez  $27, 8f
    nop
    lui   $27, %hi(QUAKE_PRESENT_DISPLAY)
    lw    $27, %lo(QUAKE_PRESENT_DISPLAY)($27)
    nop
    beqz  $27, 7f
    nop
    sw    $27, 0x1814($26)
7:
    # GP1(02h): acknowledge the flag after the flip, before the kick whose
    # own GP0(1Fh) raises it again.
    lui   $27, 0x0200
    sw    $27, 0x1814($26)
    lui   $27, 0x0400
    ori   $27, $27, 0x0002
    sw    $27, 0x1814($26)
    lw    $27, 0x10f0($26)
    nop
    ori   $27, $27, 0x0800
    sw    $27, 0x10f0($26)
    lui   $27, %hi(QUAKE_PRESENT_HEAD)
    lw    $27, %lo(QUAKE_PRESENT_HEAD)($27)
    nop
    sw    $27, 0x10a0($26)
    sw    $zero, 0x10a4($26)
    lui   $27, 0x0100
    ori   $27, $27, 0x0401
    sw    $27, 0x10a8($26)
    lui   $26, %hi(QUAKE_PRESENT_HEAD)
    sw    $zero, %lo(QUAKE_PRESENT_HEAD)($26)
    lui   $26, %hi(QUAKE_PRESENT_KICKS)
    lw    $27, %lo(QUAKE_PRESENT_KICKS)($26)
    nop
    addiu $27, $27, 1
    b     9f
    sw    $27, %lo(QUAKE_PRESENT_KICKS)($26)
8:
    lui   $26, %hi(QUAKE_PRESENT_SKIPPED)
    lw    $27, %lo(QUAKE_PRESENT_SKIPPED)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(QUAKE_PRESENT_SKIPPED)($26)
.endif
9:
    j     __psx_rt_exception_handler
    nop
    .set reorder
"#,
        probe = const PROBE,
        present = const PRESENT,
    );

    /// Point the exception vector at the handler above, after psx-rt has
    /// installed and enabled its own, and raise the draw-done flag once so
    /// the first queued frame's edge finds it set.
    #[cfg(target_arch = "mips")]
    pub fn install() {
        const EXCEPTION_VECTOR: *mut u32 = 0x8000_0080 as *mut u32;
        const J_OPCODE: u32 = 0x0800_0000;
        // Nothing is queued and channel 2 is idle during start-up.
        #[cfg(feature = "present-queue")]
        psx_gpu::signal_draw_done();
        unsafe {
            let handler: u32;
            core::arch::asm!(
                "lui {0}, %hi(quake_exception_handler)",
                "addiu {0}, {0}, %lo(quake_exception_handler)",
                out(reg) handler,
            );
            // SAFETY: the handler uses only $k0/$k1 and jumps into psx-rt's
            // with $sp untouched, so an interrupt is safe on a scratchpad
            // stack. (The pointer type comes from the parameter; the source
            // gate rejects spelling out a foreign ABI.)
            psx_rt::interrupts::declare_stack_safe_handler(core::mem::transmute::<usize, _>(
                handler as usize,
            ));
            write_volatile(EXCEPTION_VECTOR, J_OPCODE | ((handler >> 2) & 0x03ff_ffff));
            write_volatile(EXCEPTION_VECTOR.add(1), 0);
        }
        psx_rt::cache::flush_i_cache();
    }

    #[cfg(not(target_arch = "mips"))]
    pub fn install() {}

    #[inline]
    pub fn slot_full() -> bool {
        unsafe { read_volatile(addr_of!(QUAKE_PRESENT_HEAD)) != 0 }
    }

    const CHCR2: *const u32 = 0x1f80_10a8 as *const u32;

    #[inline]
    fn channel_busy() -> bool {
        #[cfg(target_arch = "mips")]
        return unsafe { read_volatile(CHCR2) } & (1 << 24) != 0;
        #[cfg(not(target_arch = "mips"))]
        false
    }

    /// Block until the handler has kicked the queued frame. Only spins when
    /// the CPU is a whole frame ahead of presentation. Not inlined, so a
    /// profile can tell the spin from work.
    #[inline(never)]
    pub fn wait_slot_empty() {
        let start = psx_rt::interrupts::vblank_count();
        while slot_full() {
            if psx_rt::interrupts::vblank_count().wrapping_sub(start) >= STALL_VBLANKS {
                release_stalled_slot();
            }
        }
    }

    /// Wait until the arena the next frame is about to rebuild is no longer
    /// being walked. That arena belongs to the frame before the most recently
    /// published one: once the published frame has been kicked (slot empty)
    /// the handler saw that frame's GP0(1Fh), so its walk had ended, and while
    /// it is still queued the only walk that can be running is the older
    /// frame's.
    #[inline(never)]
    pub fn wait_arena_free() {
        let start = psx_rt::interrupts::vblank_count();
        while slot_full() && channel_busy() {
            if psx_rt::interrupts::vblank_count().wrapping_sub(start) >= STALL_VBLANKS {
                release_stalled_slot();
            }
        }
        // Keep the arena's rebuild stores after the completion read.
        #[cfg(target_arch = "mips")]
        unsafe {
            core::arch::asm!("", options(nostack, preserves_flags));
        }
    }

    /// A queued frame has waited [`STALL_VBLANKS`] edges for the chain ahead
    /// of it: that chain wedged, or its GP0(1Fh) never ran. With the VBlank
    /// IRQ masked (so the handler cannot kick in between), stop a walk still
    /// running the way `submit_linked_list_wait` recovers, then raise the flag
    /// through the port, now that channel 2 is idle, so the next edge kicks
    /// the queued frame.
    #[cold]
    #[inline(never)]
    fn release_stalled_slot() {
        let mask = psx_io::irq::mask();
        psx_io::irq::set_mask(0);
        if slot_full() && !psx_gpu::draw_done() {
            if channel_busy() {
                psx_gpu::submit_linked_list_wait();
            }
            psx_gpu::signal_draw_done();
            unsafe {
                write_volatile(
                    addr_of_mut!(QUAKE_PRESENT_RECOVERIES),
                    read_volatile(addr_of!(QUAKE_PRESENT_RECOVERIES)).wrapping_add(1),
                );
            }
        }
        psx_io::irq::set_mask(mask);
    }

    /// Hand one chain (and the flip that exposes the previous frame) to the
    /// handler. The slot must be empty.
    #[inline]
    pub fn publish(head: *const u32, display: u32) {
        #[cfg(target_arch = "mips")]
        unsafe {
            // Publish the packet and preamble stores before the handler can
            // read the slot.
            core::arch::asm!("", options(nostack, preserves_flags));
            write_volatile(addr_of_mut!(QUAKE_PRESENT_DISPLAY), display);
            write_volatile(addr_of_mut!(QUAKE_PRESENT_HEAD), head as u32);
        }
        #[cfg(not(target_arch = "mips"))]
        let _ = (head, display);
    }
}

/// The new back buffer's GPU state, as the first two nodes of the chain the
/// VBlank handler kicks: draw area and offset (only after a swap), then the
/// world draw mode and the clear. One per arena, since the handler may still
/// be walking the other one.
#[cfg(feature = "present-queue")]
#[repr(C, align(4))]
struct Preamble {
    target_tag: u32,
    draw_area_top_left: u32,
    draw_area_bottom_right: u32,
    draw_offset: u32,
    clear_tag: u32,
    draw_mode: u32,
    texture_window: u32,
    fill: u32,
    fill_xy: u32,
    fill_wh: u32,
}

#[cfg(feature = "present-queue")]
const EMPTY_PREAMBLE: Preamble = Preamble {
    target_tag: 0,
    draw_area_top_left: 0,
    draw_area_bottom_right: 0,
    draw_offset: 0,
    clear_tag: 0,
    draw_mode: 0,
    texture_window: 0,
    fill: 0,
    fill_xy: 0,
    fill_wh: 0,
};

#[cfg(feature = "present-queue")]
static mut PREAMBLES: [Preamble; 2] = [EMPTY_PREAMBLE, EMPTY_PREAMBLE];

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
        #[cfg(feature = "present-queue")]
        core::ptr::write_volatile(addr_of_mut!(present_queue::QUAKE_PRESENT_HEAD), 0);
        FRAME_BUFFER = FrameBuffer::new_strided(WIDTH, HEIGHT, BACK_BUFFER_Y);
        psx_gpu::set_draw_area(0, 0, WIDTH - 1, HEIGHT - 1);
        psx_gpu::set_draw_offset(0, 0);
        configure_world_material();
        psx_gpu::fill_rect(0, 0, 512, 256, 0, 0, 0);
        psx_gpu::fill_rect(0, 256, 512, 256, 0, 0, 0);
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
    #[cfg(any(feature = "present-queue", feature = "irq-epc-probe"))]
    present_queue::install();
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
        #[cfg(feature = "present-queue")]
        present_queue::wait_arena_free();
        build_ot().clear();
        // Every queued chain ends on the shared GP0(1Fh) node, which raises
        // the draw-done flag the VBlank handler flips on.
        #[cfg(feature = "present-queue")]
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
            build_ot().insert_tagged_packet_stream_unchecked(packet_start, packet_end);
        }

        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::stage_end(psx_telemetry::stage::OT_SUBMIT);
    }
    #[cfg(feature = "present-queue")]
    unsafe {
        queue_frame()
    }
    #[cfg(not(feature = "present-queue"))]
    unsafe {
        present_blocking()
    }
}

/// Publish the built frame to the VBlank handler and return without waiting
/// for the edge. The GP0 stream is word-for-word the blocking path's.
#[cfg(feature = "present-queue")]
unsafe fn queue_frame() {
    use psx_gpu::material::TextureMaterial;

    present_queue::wait_slot_empty();
    let swap = unsafe { GPU_SUBMISSION_PENDING };
    let fb = unsafe { framebuffer() };
    // The flip exposes the previous frame, whose buffer is the current draw
    // target; this frame then draws into the other one.
    let display = if swap {
        let word = 0x0500_0000 | (u32::from(fb.buffer_y(fb.drawing)) << 10);
        fb.drawing ^= 1;
        word
    } else {
        0
    };
    let target_y = u32::from(fb.buffer_y(fb.drawing));
    let material = TextureMaterial::new(0, 0).with_dither(true);
    let ot_head = build_ot().submit_head() as u32;
    let preamble = unsafe { &mut *addr_of_mut!(PREAMBLES).cast::<Preamble>().add(BUILD_BUFFER) };
    preamble.draw_area_top_left = 0xE300_0000 | (target_y << 10);
    preamble.draw_area_bottom_right =
        0xE400_0000 | u32::from(WIDTH - 1) | ((target_y + u32::from(HEIGHT) - 1) << 10);
    preamble.draw_offset = 0xE500_0000 | ((target_y & 0x7FF) << 11);
    preamble.draw_mode = material.draw_mode_word();
    preamble.texture_window = material.texture_window_word();
    preamble.fill = 0x0200_0000;
    preamble.fill_xy = target_y << 16;
    preamble.fill_wh = (u32::from(HEIGHT) << 16) | u32::from(WIDTH);
    preamble.clear_tag = (5 << 24) | (ot_head & 0x00FF_FFFF);
    preamble.target_tag = (3 << 24) | (addr_of!(preamble.clear_tag) as u32 & 0x00FF_FFFF);

    let mut head = addr_of!(preamble.target_tag);
    if !swap {
        // Nothing was flipped, so the draw target already matches (boot, or
        // after gpu_present_pending_frame's swap).
        head = addr_of!(preamble.clear_tag);
    }
    if unsafe { DEFERRED_UPLOAD_COUNT } != 0 {
        // Image loads go through the GP0 FIFO, so the previous frame must be
        // off the GPU first. The slot is empty, so the handler cannot kick
        // anything while the CPU owns GP0. Moving the draw target here keeps
        // the stream in the blocking path's order.
        if swap {
            psx_gpu::submit_linked_list_wait();
            psx_gpu::draw_sync();
            psx_io::gpu::write_gp0(preamble.draw_area_top_left);
            psx_io::gpu::write_gp0(preamble.draw_area_bottom_right);
            psx_io::gpu::write_gp0(preamble.draw_offset);
            head = addr_of!(preamble.clear_tag);
        }
        unsafe { flush_deferred_vram_uploads() };
    }
    #[cfg(feature = "emulator-telemetry")]
    psx_telemetry::emit::counter(psx_telemetry::counter::VISUAL_FRAMES, 1);

    let ot = unsafe { build_ot() };
    unsafe {
        ot.insert_packed_commands_reverse_unchecked(
            addr_of!(SCREEN_COMMANDS).cast::<usize>(),
            SCREEN_COMMAND_COUNT,
        );
    }
    present_queue::publish(head, display);
    unsafe {
        GPU_SUBMISSION_PENDING = true;
    }
}

/// Wait for the previous frame and the next vblank edge, flip, and kick this
/// frame.
#[cfg(not(feature = "present-queue"))]
unsafe fn present_blocking() {
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

/// Last CD failure snapshot for the on-screen loading error.
pub fn storage_diag() -> u32 {
    unsafe { (&*addr_of!(READER)).diag() }
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
