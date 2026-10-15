//! Monster position and state trace on the guest debug log.
//!
//! Spawn placement is the first thing a player sees of a monster, and the
//! population drifts with the AI after that. This probe prints one line per
//! monster the first frame it exists, and another whenever it moves 8 units,
//! changes state, acquires its enemy or gains or loses its footing. The host
//! builder parses the lines (`cargo quake-build e1m1-spawn-regress`); a human
//! can read them to see where a monster actually stood at tick N.
//!
//! Line format (all coordinates in whole map units, `t` in 60 Hz ticks):
//!
//! ```text
//! SP t=<tick> s=<source> a=<authored x,y,z> o=<x>,<y>,<z> yaw=<0..4095> st=<state> act=<0|1> bt=<0|1> hp=<health> pl=<x>,<y>,<z>
//! ```
//!
//! `bt` is 0 only for a walking monster whose box hangs off an edge
//! (`SV_CheckBottom` fails), which the original never lets happen.

use alloc::format;
use alloc::vec::Vec;

use quake_core::monster::MonsterState;
use quake_formats::Vec3I32;

use crate::asset::ResidentMap;
use crate::entity::EntityScene;

/// A step this large (in units, either axis or height) is worth a line.
const MOVE_THRESHOLD_UNITS: i32 = 8;

#[derive(Copy, Clone)]
struct Last {
    source: u16,
    origin: Vec3I32,
    state: MonsterState,
    active: bool,
    supported: bool,
}

struct Probe {
    tick: u32,
    last: Vec<Last>,
}

static mut PROBE: Probe = Probe {
    tick: 0,
    last: Vec::new(),
};

fn units(value: i32) -> i32 {
    value >> 12
}

/// Call once per simulated frame after the monsters have thought.
pub fn observe(entities: &EntityScene, world: &ResidentMap, player: Vec3I32, elapsed_ticks: u16) {
    if cfg!(feature = "e1m1-save-regression") {
        return;
    }
    // SAFETY: the guest is single threaded; the same pattern the regression
    // probes use.
    let probe = unsafe { &mut *core::ptr::addr_of_mut!(PROBE) };
    if probe.last.capacity() == 0 {
        probe.last.reserve(128);
    }
    for row in entities.spawn_probe_rows() {
        let supported = !row.walks || entities.spawn_probe_supported(world, row.origin, row.hull);
        let previous = probe
            .last
            .iter_mut()
            .find(|last| last.source == row.source_index);
        let moved = |last: &Last| {
            let dx = units(row.origin.x - last.origin.x).abs();
            let dy = units(row.origin.y - last.origin.y).abs();
            let dz = units(row.origin.z - last.origin.z).abs();
            dx >= MOVE_THRESHOLD_UNITS || dy >= MOVE_THRESHOLD_UNITS || dz >= MOVE_THRESHOLD_UNITS
        };
        let report = match &previous {
            None => true,
            Some(last) => {
                moved(last)
                    || last.state != row.state
                    || last.active != row.active
                    || last.supported != supported
            }
        };
        if !report {
            continue;
        }
        let authored = world
            .entities()
            .get(usize::from(row.source_index))
            .map(|entity| entity.origin)
            .unwrap_or_default();
        let line = format!(
            "SP t={} s={} a={},{},{} o={},{},{} yaw={} st={:?} act={} bt={} hp={} pl={},{},{}",
            probe.tick,
            row.source_index,
            units(authored.x),
            units(authored.y),
            units(authored.z),
            units(row.origin.x),
            units(row.origin.y),
            units(row.origin.z),
            i32::from(row.yaw) & 0x0fff,
            row.state,
            u8::from(row.active),
            u8::from(supported),
            row.health,
            units(player.x),
            units(player.y),
            units(player.z),
        );
        psx_telemetry::emit::debug_log(&line);
        let entry = Last {
            source: row.source_index,
            origin: row.origin,
            state: row.state,
            active: row.active,
            supported,
        };
        match previous {
            Some(last) => *last = entry,
            None => probe.last.push(entry),
        }
    }
    probe.tick = probe.tick.wrapping_add(u32::from(elapsed_ticks));
}

/// Idle-scene player placement. Without a script the player stays at the map's
/// own spawn; `spawn-script-bridge` moves it, after two seconds, to the west
/// platform of E1M1's t2 bridge looking east across the gap at the far
/// platform, where the player stands when the button is pressed.
#[cfg(feature = "e1m1-spawn-regression")]
pub fn prepare(player: &mut crate::player::Player) {
    #[cfg(feature = "spawn-script-bridge")]
    {
        // SAFETY: single guest thread.
        let tick = unsafe { (*core::ptr::addr_of!(PROBE)).tick };
        if tick >= 120 {
            player.place_camera(
                Vec3I32 {
                    x: -40 << 12,
                    y: 2720 << 12,
                    z: -34 << 12,
                },
                [0, 0, 0],
            );
        }
    }
    #[cfg(not(feature = "spawn-script-bridge"))]
    let _ = player;
}
