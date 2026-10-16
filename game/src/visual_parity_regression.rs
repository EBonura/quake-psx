//! Fixed-camera image and packet regression.

#![allow(dead_code)]

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use crate::renderer::{Camera, RenderStats};
use quake_formats::Vec3I32;

include!(concat!(env!("OUT_DIR"), "/visual_parity_camera.rs"));

pub const PROBE_MAGIC: u32 = 0x5156_4953;
pub const PROBE_VERSION: u32 = 2;

#[repr(C)]
pub struct VisualParityProbe {
    pub magic: u32,
    pub version: u32,
    pub frames: u32,
    pub packets: u32,
    pub hardware_triangles: u32,
    pub windowed_packets: u32,
    pub window_resets: u32,
    pub reset_failures: u32,
    pub overflow_frames: u32,
    pub view_model_packets: u32,
    pub view_model_registered_packets: u32,
    pub hud_packets: u32,
    pub hud_registered_packets: u32,
    pub crosshair_registered_packets: u32,
    pub screen_registered_packets: u32,
}

#[no_mangle]
pub static mut QUAKE_VISUAL_PARITY_PROBE: VisualParityProbe = VisualParityProbe {
    magic: PROBE_MAGIC,
    version: PROBE_VERSION,
    frames: 0,
    packets: 0,
    hardware_triangles: 0,
    windowed_packets: 0,
    window_resets: 0,
    reset_failures: 0,
    overflow_frames: 0,
    view_model_packets: 0,
    view_model_registered_packets: 0,
    hud_packets: 0,
    hud_registered_packets: 0,
    crosshair_registered_packets: 0,
    screen_registered_packets: 0,
};

#[cfg(not(feature = "probe-poses"))]
pub const fn camera() -> Camera {
    Camera {
        origin: Vec3I32 {
            x: CAMERA_ORIGIN_Q12[0],
            y: CAMERA_ORIGIN_Q12[1],
            z: CAMERA_ORIGIN_Q12[2],
        },
        angles: CAMERA_ANGLES,
    }
}

pub fn observe_render(stats: RenderStats) {
    unsafe {
        let probe = addr_of_mut!(QUAKE_VISUAL_PARITY_PROBE);
        write_volatile(
            addr_of_mut!((*probe).frames),
            read_volatile(addr_of_mut!((*probe).frames)).wrapping_add(1),
        );
        write_volatile(
            addr_of_mut!((*probe).packets),
            read_volatile(addr_of_mut!((*probe).packets)).wrapping_add(stats.packets),
        );
        write_volatile(
            addr_of_mut!((*probe).hardware_triangles),
            read_volatile(addr_of_mut!((*probe).hardware_triangles))
                .wrapping_add(stats.hardware_triangles),
        );
        write_volatile(
            addr_of_mut!((*probe).windowed_packets),
            read_volatile(addr_of_mut!((*probe).windowed_packets))
                .wrapping_add(stats.scoped_window_packets),
        );
        write_volatile(
            addr_of_mut!((*probe).window_resets),
            read_volatile(addr_of_mut!((*probe).window_resets))
                .wrapping_add(stats.scoped_window_resets),
        );
        write_volatile(
            addr_of_mut!((*probe).reset_failures),
            read_volatile(addr_of_mut!((*probe).reset_failures))
                .wrapping_add(stats.scoped_window_reset_failures),
        );
        if stats.packet_overflow_avoided {
            write_volatile(
                addr_of_mut!((*probe).overflow_frames),
                read_volatile(addr_of_mut!((*probe).overflow_frames)).wrapping_add(1),
            );
        }
        write_volatile(
            addr_of_mut!((*probe).view_model_packets),
            stats.view_model_packets,
        );
        write_volatile(
            addr_of_mut!((*probe).view_model_registered_packets),
            stats.view_model_registered_packets,
        );
        write_volatile(addr_of_mut!((*probe).hud_packets), stats.hud_packets);
        write_volatile(
            addr_of_mut!((*probe).hud_registered_packets),
            stats.hud_registered_packets,
        );
        write_volatile(
            addr_of_mut!((*probe).crosshair_registered_packets),
            stats.crosshair_registered_packets,
        );
        write_volatile(
            addr_of_mut!((*probe).screen_registered_packets),
            stats.screen_registered_packets,
        );
    }
}

/// The probe script, `QUAKE_PSX_PROBE_POSES` at build time: poses separated by
/// `;`, each `x y z pitch yaw roll hold [mN=x:y:z ...]`: origin in whole Quake
/// units, angles in 4096ths of a turn (what a frame's camera holds), `hold`
/// frames long, followed by any number of brush-model origin overrides in
/// whole units (`m3=0:0:-120`). Once the script ends its last pose holds.
#[cfg(feature = "probe-poses")]
const SCRIPT: &str = match option_env!("QUAKE_PSX_PROBE_POSES") {
    Some(script) => script,
    None => "",
};

/// `QUAKE_PSX_PROBE_BIAS` at build time: the `bias=` of poses that name none.
#[cfg(feature = "probe-poses")]
const DEFAULT_BIAS: u16 = match option_env!("QUAKE_PSX_PROBE_BIAS") {
    Some(text) => {
        let bytes = text.as_bytes();
        let mut value = 0u16;
        let mut index = 0;
        while index < bytes.len() {
            value = value * 10 + (bytes[index] - b'0') as u16;
            index += 1;
        }
        value
    }
    None => 0,
};

/// Build with `QUAKE_PSX_PROBE_NOWATER` set to draw opaque water.
#[cfg(feature = "probe-poses")]
pub const NO_WATER: bool = option_env!("QUAKE_PSX_PROBE_NOWATER").is_some();

#[cfg(feature = "probe-poses")]
fn frames_rendered() -> u32 {
    unsafe { read_volatile(addr_of_mut!(QUAKE_VISUAL_PARITY_PROBE.frames)) }
}

/// The pose the current frame draws.
#[cfg(feature = "probe-poses")]
fn current_pose() -> &'static str {
    let frame = frames_rendered();
    let mut start = 0u32;
    let mut last = "";
    for pose in SCRIPT.split(';') {
        let pose = pose.trim();
        if pose.is_empty() {
            continue;
        }
        last = pose;
        let hold = pose
            .split_whitespace()
            .nth(6)
            .and_then(|field| field.parse::<u32>().ok())
            .unwrap_or(1);
        if frame < start + hold {
            return pose;
        }
        start += hold;
    }
    last
}

#[cfg(feature = "probe-poses")]
pub fn camera() -> Camera {
    let mut fields = current_pose().split_whitespace();
    let mut next = || fields.next().and_then(|f| f.parse::<i32>().ok()).unwrap_or(0);
    let origin = Vec3I32 {
        x: next() << 12,
        y: next() << 12,
        z: next() << 12,
    };
    Camera {
        origin,
        angles: [next() as i16, next() as i16, next() as i16],
    }
}

/// Set by a pose's `nocull` token: every brush model in the frustum is drawn
/// whatever the PVS says, the reference a visibility miss is measured against.
#[cfg(feature = "probe-poses")]
pub static mut NO_BRUSH_CULL: bool = false;

/// Set by a pose's `bias=N` token: brush-model packets sort N slots nearer.
#[cfg(feature = "probe-poses")]
pub static mut BRUSH_BIAS: u16 = 0;

/// Set by a pose's `tint=N` token: brush model N is drawn in flat green (its
/// vertex colour; the texture still modulates it), so a screen capture says
/// exactly which pixels it covers.
#[cfg(feature = "probe-poses")]
pub static mut TINT_MODEL: u8 = 0;

/// Put every scripted brush model where the current pose says.
#[cfg(feature = "probe-poses")]
pub fn apply_movers(entities: &mut crate::entity::EntityScene, map: &crate::asset::ResidentMap) {
    unsafe {
        NO_BRUSH_CULL = current_pose().split_whitespace().any(|field| field == "nocull");
        BRUSH_BIAS = current_pose()
            .split_whitespace()
            .find_map(|field| field.strip_prefix("bias=")?.parse::<u16>().ok())
            .unwrap_or(DEFAULT_BIAS);
        TINT_MODEL = current_pose()
            .split_whitespace()
            .find_map(|field| field.strip_prefix("tint=")?.parse::<u8>().ok())
            .unwrap_or(0);
    }
    for field in current_pose().split_whitespace().skip(7) {
        let Some(rest) = field.strip_prefix('m') else {
            continue;
        };
        let Some((index, origin)) = rest.split_once('=') else {
            continue;
        };
        let mut parts = origin.split(':');
        let mut next = || parts.next().and_then(|p| p.parse::<i32>().ok()).unwrap_or(0);
        let origin = Vec3I32 {
            x: next() << 12,
            y: next() << 12,
            z: next() << 12,
        };
        if let Ok(index) = index.parse::<u8>() {
            entities.probe_set_brush_origin(map, index, origin);
        }
    }
}

/// A pose's `vm=MODEL:FRAME` token (hex model id, frame number): the view
/// model is drawn from that model and frame whatever the player holds.
#[cfg(feature = "probe-poses")]
pub fn view_model_override() -> Option<quake_core::combat::WeaponView> {
    let field = current_pose().split_whitespace().find_map(|field| field.strip_prefix("vm="))?;
    let (model, frame) = field.split_once(':')?;
    Some(quake_core::combat::WeaponView {
        model_id: i16::from_str_radix(model, 16).ok()?,
        frame: frame.parse().ok()?,
    })
}

/// Build with `QUAKE_PSX_PROBE_BLEND_NAILGUNS` set to blend the nailguns' frames like any other pose (the behaviour before they were stepped).
#[cfg(feature = "probe-poses")]
pub const BLEND_NAILGUNS: bool = option_env!("QUAKE_PSX_PROBE_BLEND_NAILGUNS").is_some();

/// Build with `QUAKE_PSX_PROBE_OLD_CULL` set to cull a brush model whose centre leaf is out of the PVS even when its box touches a visible leaf (the behaviour before the Quake rule).
#[cfg(feature = "probe-poses")]
pub const OLD_CULL: bool = option_env!("QUAKE_PSX_PROBE_OLD_CULL").is_some();


