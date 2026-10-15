// SPDX-License-Identifier: GPL-2.0-or-later
//! Sporadic dark-ambient music, played from the game's own XA-ADPCM files.
//!
//! The disc carries `MUSIC1.XA` and `MUSIC2.XA` (eight loops, four to a file,
//! packed by `psx-audio-cook xa-pack`; `music_map.rs` is the table the
//! builder generates with them). The drive decodes the loop and feeds the
//! SPU's CD input, so the CPU only starts and stops it.
//!
//! The drive cannot loop an XA file without a seek, and the seek is an audible
//! gap, so each loop sits in its channel `XA_REPEATS` times end to end (as many
//! times as the schedule can ask for it) and a song is one uninterrupted read:
//! there is nothing to poll and nothing to restart. The schedule decides when
//! the read is cut off.
//!
//! What plays and when is [`quake_core::ambient::Schedule`]: a stretch of
//! silence after a map starts, then one loop fades in, plays one to three
//! times and fades out. This module turns its steps into drive commands and
//! SPU volume writes.
//!
//! A song is a read, so any data read preempts it. [`Music::suspend_for_load`]
//! stops the drive before every map load, and [`Music::resume_after_load`]
//! restarts the schedule from the top afterwards: nothing resumes in the
//! middle of a song, so no state has to survive a read.

use psx_io::cd::xa::{DriveSpeed, File, Player};
use psx_io::periph::Cd;
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use quake_core::ambient::{Schedule, Step, FADE_MAX, TICKS_PER_SECOND};
use quake_core::menu::{DEFAULT_MUSIC_VOLUME, VOLUME_STEPS};

include!("music_map.rs");

/// The drive's own mixer level (`0x80` is unity). The SPU's CD volume carries
/// the fades and the Options slider.
const DRIVE_VOLUME: u8 = 0x80;

const SONG_COUNT: usize = XA_SONGS.len();

/// Length of each loop in ticks, from the encoder's song table, so the
/// schedule times a loop by the audio it actually carries.
const SONG_TICKS: [u32; SONG_COUNT] = {
    let mut ticks = [0; SONG_COUNT];
    let mut song = 0;
    while song < SONG_COUNT {
        ticks[song] = XA_SONGS[song].2 / XA_REPEATS * TICKS_PER_SECOND / 1000;
        song += 1;
    }
    ticks
};

/// Seed the private stream mixes the boot tick into.
const SEED_SALT: u32 = 0x2f6e_2b1d;

/// The shipping silences; the gate image shortens them (see the
/// `music-trace-fast` feature) so a loop is up within seconds of a map start.
const fn schedule() -> Schedule {
    let schedule = Schedule::new(SEED_SALT, &SONG_TICKS);
    #[cfg(feature = "music-trace-fast")]
    let schedule = schedule.with_waits((4, 6), (8, 12));
    schedule
}

pub struct Music {
    /// The disc really does carry the music files.
    available: bool,
    /// The player has not turned it off.
    enabled: bool,
    /// Original `bgmvolume`, in the Options menu's `0..=10` slider steps.
    volume: u8,
    /// Fade level (`0..=FADE_MAX`) the schedule last asked for.
    fade: u16,
    /// Have we looked for the files yet?
    probed: bool,
    /// The schedule's stream has been seeded from the clock.
    seeded: bool,
    /// First sector and sector count of each file.
    files: [(u32, u32); XA_FILE_COUNT],
    /// The SDK's XA player; it owns the CD token once the files are found.
    player: Option<Player>,
    schedule: Schedule,
}

impl Music {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            available: false,
            enabled: true,
            volume: DEFAULT_MUSIC_VOLUME,
            fade: 0,
            probed: false,
            seeded: false,
            files: [(0, 0); XA_FILE_COUNT],
            player: None,
            schedule: schedule(),
        }
    }

    /// Is there music to talk about? The Options page hides its rows when not,
    /// rather than offering switches that do nothing.
    #[optimize(size)]
    pub const fn available(&self) -> bool {
        self.available
    }

    #[optimize(size)]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Adopt the Options page's music rows. The menu is refreshed from this
    /// module every frame, so a difference here is a fresh player edit.
    #[optimize(size)]
    #[inline(never)]
    pub fn apply_menu(&mut self, on: bool, volume: u8, tick: u32) {
        self.set_volume(volume.min(VOLUME_STEPS));
        if !self.available {
            self.enabled = on;
            return;
        }
        if on != self.enabled {
            self.set_enabled(on, tick);
        }
    }

    /// The port's calibrated CD ceiling remains one half of hardware gain;
    /// the original ten-step `bgmvolume` slider scales within that range and
    /// the schedule's fade scales within the slider.
    #[optimize(size)]
    fn set_volume(&mut self, level: u8) {
        if level == self.volume {
            return;
        }
        self.volume = level;
        self.write_gain();
    }

    #[optimize(size)]
    fn write_gain(&self) {
        let numerator = u16::from(self.volume) * self.fade;
        let denominator = u16::from(VOLUME_STEPS) * 2 * FADE_MAX;
        let gain = psx_spu::CdVolume::linear(numerator, denominator);
        psx_spu::set_cd_volume(gain, gain);
    }

    #[optimize(size)]
    fn set_fade(&mut self, level: u16) {
        self.fade = level;
        self.write_gain();
    }

    /// Turn the music on or off. Off stops the drive rather than only
    /// declining to start another loop, or the current one would play on under
    /// a menu that claims it is off. On starts the silence over.
    #[optimize(size)]
    #[inline(never)]
    fn set_enabled(&mut self, on: bool, tick: u32) {
        self.enabled = on;
        if on {
            self.restart(tick);
        } else {
            self.silence();
        }
    }

    /// Stop the drive and put the schedule and the volume at rest.
    #[optimize(size)]
    #[inline(never)]
    fn silence(&mut self) {
        self.stop_drive();
        self.schedule.halt();
        self.set_fade(0);
    }

    #[optimize(size)]
    fn restart(&mut self, tick: u32) {
        if !self.seeded {
            // The boot's own timing (intro, loads, how long the player sat in
            // the menu) differs run to run; nothing the simulation reads does.
            self.schedule
                .reseed(tick.wrapping_mul(0x9e37_79b9) ^ SEED_SALT);
            self.seeded = true;
        }
        self.schedule.restart(tick);
    }

    /// Hand the drive back before a level load: stop the loop and the
    /// schedule. The first call also looks for the music files, while the
    /// loading picture is already up and the drive is about to be read anyway.
    #[optimize(size)]
    #[inline(never)]
    pub fn suspend_for_load(&mut self) {
        if !self.probed {
            self.probed = true;
            self.available = self.locate_files();
        }
        if !self.available {
            return;
        }
        #[cfg(feature = "music-trace")]
        trace(
            "suspend",
            psx_rt::interrupts::vblank_count(),
            self.schedule.playing().map_or(u32::MAX, u32::from),
            u32::from(self.fade),
        );
        self.silence();
    }

    /// Start the schedule over after a level load. A new map is where the
    /// silence begins again; nothing is resumed.
    #[optimize(size)]
    #[inline(never)]
    pub fn resume_after_load(&mut self, tick: u32) {
        if !self.available || !self.enabled {
            return;
        }
        self.restart(tick);
        #[cfg(feature = "music-trace")]
        trace("restart", tick, u32::MAX, 0);
    }

    /// One tick: run the schedule.
    #[optimize(size)]
    #[inline(never)]
    pub fn update(&mut self, tick: u32) {
        if !self.available || !self.enabled {
            return;
        }
        match self.schedule.update(tick) {
            Step::Idle => {}
            Step::Start(song) => self.begin(song, tick),
            Step::Fade(level) => {
                self.set_fade(level);
                #[cfg(feature = "music-trace")]
                trace("fade", tick, u32::MAX, u32::from(level));
            }
            Step::Stop => {
                self.stop_drive();
                self.set_fade(0);
                #[cfg(feature = "music-trace")]
                trace("stop", tick, u32::MAX, 0);
            }
        }
    }

    /// Start loop `song` from the top, silent; the schedule's fade raises it.
    #[optimize(size)]
    #[inline(never)]
    fn begin(&mut self, song: u8, tick: u32) {
        self.set_fade(0);
        let (file, channel, _) = XA_SONGS[song as usize];
        let (lba, sectors) = self.files[file as usize];
        // The song is one uninterrupted read (see the module note), so the
        // player never polls and the sector count only describes the file.
        let file = File::new(
            lba,
            sectors,
            XA_FILE_NUMBERS[file as usize],
            if XA_SPEED_DOUBLE {
                DriveSpeed::Double
            } else {
                DriveSpeed::Single
            },
        );
        let started = match self.player.as_mut() {
            Some(player) => {
                player.set_volume(DRIVE_VOLUME, DRIVE_VOLUME);
                player.play(file.song(channel), false).is_ok()
            }
            None => false,
        };
        #[cfg(feature = "music-trace")]
        trace(
            if started { "start" } else { "start-refused" },
            tick,
            u32::from(song),
            0,
        );
        if started {
        } else {
            // The drive refused a command. Give the loop up and try again
            // after the next silence rather than hammering a busy drive.
            self.schedule.halt();
            self.schedule.restart(tick);
        }
    }

    /// Pause the drive if a song is being read.
    #[optimize(size)]
    #[inline(never)]
    fn stop_drive(&mut self) {
        if let Some(player) = self.player.as_mut() {
            player.stop();
        }
    }

    /// Find the music files by name in the disc's root directory, so no sector
    /// number is baked into the executable, and take the CD token the player
    /// drives. False if any file is missing (a disc built without music).
    #[optimize(size)]
    #[inline(never)]
    fn locate_files(&mut self) -> bool {
        let mut sector = [0u32; SECTOR_WORDS];
        let mut reader = SectorReader::new();
        // SAFETY: the loader's own reader is idle here (this runs at a level
        // boundary) and nothing else holds the controller.
        unsafe {
            if !reader.prepare() {
                return false;
            }
            let Some(volume) = read_sector(&mut reader, PVD_LBA, &mut sector) else {
                reader.stop();
                return false;
            };
            let Some((root, size)) = root_directory(volume) else {
                reader.stop();
                return false;
            };
            let mut found = [false; XA_FILE_COUNT];
            // A root directory this small is one sector; scan a few in case not.
            for at in 0..size.div_ceil(2048).min(4) {
                let Some(directory) = read_sector(&mut reader, root + at, &mut sector) else {
                    break;
                };
                for (file, name) in XA_FILE_NAMES.iter().enumerate() {
                    if let Some((lba, bytes)) = find_in_directory(directory, name) {
                        self.files[file] = (lba, bytes / 2048);
                        found[file] = true;
                    }
                }
            }
            reader.stop();
            if !found.iter().all(|f| *f) {
                return false;
            }
            self.player = Some(Player::new(Cd::steal()));
        }
        // The SPU's CD input is routed once at boot (`quake.rs`); the volume
        // is this module's from here on.
        true
    }
}

/// ISO 9660: the primary volume descriptor's sector.
const PVD_LBA: u32 = 16;

fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Root directory `(lba, size_bytes)` from the primary volume descriptor.
fn root_directory(pvd: &[u8]) -> Option<(u32, u32)> {
    if pvd.len() < 190 || pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return None;
    }
    let record = &pvd[156..190];
    Some((le32(&record[2..6]), le32(&record[10..14])))
}

/// Find `name` in one sector of directory records, ignoring case and the
/// `;1` version suffix. Returns `(lba, size_bytes)`.
fn find_in_directory(sector: &[u8], name: &str) -> Option<(u32, u32)> {
    let mut at = 0;
    while at + 33 <= sector.len() {
        let length = sector[at] as usize;
        if length == 0 || at + length > sector.len() {
            break;
        }
        let record = &sector[at..at + length];
        let name_length = record[32] as usize;
        if 33 + name_length <= length {
            let found = &record[33..33 + name_length];
            let found = found.split(|b| *b == b';').next().unwrap_or(found);
            if found.eq_ignore_ascii_case(name.as_bytes()) {
                return Some((le32(&record[2..6]), le32(&record[10..14])));
            }
        }
        at += length;
    }
    None
}

/// Read one data sector into `sector` and return its bytes.
#[optimize(size)]
#[inline(never)]
unsafe fn read_sector<'a>(
    reader: &mut SectorReader,
    lba: u32,
    sector: &'a mut [u32; SECTOR_WORDS],
) -> Option<&'a [u8]> {
    if !reader.start_read(lba) {
        return None;
    }
    let read = reader.read_sector(sector);
    reader.stop();
    read.then(|| core::slice::from_raw_parts(sector.as_ptr().cast::<u8>(), SECTOR_WORDS * 4))
}

/// One state-change line for the headless gate: `quake-music <what> tick=<n>
/// song=<n> level=<n>`; `song` is 4294967295 where none applies. Formatted into
/// a stack buffer: the target allocator never frees, so a line built on the
/// heap would drain it over a long run.
#[cfg(feature = "music-trace")]
#[inline(never)]
fn trace(what: &str, tick: u32, song: u32, level: u32) {
    use core::fmt::Write;
    struct Line {
        bytes: [u8; 96],
        length: usize,
    }
    impl Write for Line {
        fn write_str(&mut self, text: &str) -> core::fmt::Result {
            let room = self.bytes.len() - self.length;
            let take = text.len().min(room);
            self.bytes[self.length..self.length + take].copy_from_slice(&text.as_bytes()[..take]);
            self.length += take;
            Ok(())
        }
    }
    let mut line = Line {
        bytes: [0; 96],
        length: 0,
    };
    let _ = write!(
        line,
        "quake-music {what} tick={tick} song={song} level={level}"
    );
    if let Ok(text) = core::str::from_utf8(&line.bytes[..line.length]) {
        psx_telemetry::emit::debug_log(text);
    }
}
