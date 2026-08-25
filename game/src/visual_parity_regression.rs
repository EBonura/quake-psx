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
