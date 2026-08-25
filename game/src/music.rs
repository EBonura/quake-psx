// SPDX-License-Identifier: GPL-2.0-or-later
//! Optional demo-disc menu music.
//!
//! Standalone Quake has no audio tracks. On the demo disc, launcher music is
//! stored in tracks 2 through 5 and may be reused without copying audio into
//! the game. `cdda_track_base` is subtracted because the CD driver adds it
//! back when addressing the physical track.
//!
//! Map loads temporarily take ownership of the drive through
//! [`Music::suspend_for_load`] and [`Music::resume_after_load`].

use psx_io::cdda::{CddaEndDetector, CddaStarter};
use psx_io::cdrom;
use quake_core::menu::{DEFAULT_MUSIC_VOLUME, MUSIC_TRACKS, VOLUME_STEPS};

/// First CD-DA track on a mixed-mode disc: track 1 is the data track.
const FIRST_TRACK: u8 = 2;
/// How many menu songs the demo disc carries.
const TRACK_COUNT: u8 = MUSIC_TRACKS.len() as u8;
/// The disc must show at least this many tracks for the menu music to be there.
const TRACKS_NEEDED: u8 = FIRST_TRACK + TRACK_COUNT - 1;

/// How long the now-playing banner stays up once a song starts. Four seconds:
/// long enough to read twice, gone before it reads as HUD. Public so the
/// renderer can time its slide against the same clock.
pub const BANNER_TICKS: u32 = 240;

/// GetTN. Answers with the first and last track numbers, in BCD.
const GET_TN: u8 = 0x13;
/// Per-command spin budget. Emulators answer instantly; silicon does not.
const SPINS: u32 = 0x10_0000;
/// Ticks between drive status polls. A GetStat is a round trip, and this runs
/// inside a frame that is already at its deadline, so it is paced.
const POLL_TICKS: u32 = 30;
/// Consecutive quiet polls that mean the song is over rather than dipping.
const IDLE_POLLS_TO_ADVANCE: u8 = 8;
/// Ticks to wait before asking the drive anything. Quake's boot has just
/// finished loading a map through it and the head is still settling.
const PROBE_TICK: u32 = 90;

pub struct Music {
    /// The disc really does carry the menu tracks.
    available: bool,
    /// The player has not turned it off.
    enabled: bool,
    /// Which of the four is playing, 0-based.
    index: u8,
    /// Original `bgmvolume`, in the Options menu's `0..=10` slider steps.
    volume: u8,
    /// Have we asked the drive what is on it yet?
    probed: bool,
    /// Tick the current song was started on, for the now-playing banner.
    announced_at: Option<u32>,
    starter: CddaStarter,
    end: CddaEndDetector,
    next_poll: u32,
}

impl Music {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            available: false,
            enabled: true,
            index: 0,
            volume: DEFAULT_MUSIC_VOLUME,
            probed: false,
            announced_at: None,
            starter: CddaStarter::new().with_spins(SPINS),
            end: CddaEndDetector::new(IDLE_POLLS_TO_ADVANCE),
            next_poll: 0,
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

    /// Which song is current, as the menu's `TRACK` row numbers them.
    #[optimize(size)]
    pub const fn track(&self) -> u8 {
        self.index
    }

    /// The song to announce and how long it has been up, while a start is
    /// recent enough to announce. The popup follows the drive rather than the
    /// menu: switched off, there is nothing playing to name.
    #[optimize(size)]
    #[inline(never)]
    pub fn now_playing(&self, tick: u32) -> Option<(&'static str, u32)> {
        let at = self.announced_at?;
        if !self.available || !self.enabled {
            return None;
        }
        let elapsed = tick.wrapping_sub(at);
        if elapsed > BANNER_TICKS {
            return None;
        }
        Some((MUSIC_TRACKS[self.index as usize], elapsed))
    }

    /// Adopt the Options page's music rows. The menu is refreshed from this
    /// module every frame, so a difference here is a fresh player edit rather
    /// than a stale copy of a song this module advanced on its own.
    #[optimize(size)]
    #[inline(never)]
    pub fn apply_menu(&mut self, on: bool, track: u8, volume: u8, tick: u32) {
        self.set_volume(volume);
        if !self.available {
            self.enabled = on;
            return;
        }
        if track != self.index && track < TRACK_COUNT {
            self.index = track;
            if self.enabled {
                self.begin_track(tick);
            }
        }
        if on != self.enabled {
            self.set_enabled(on, tick);
        }
    }

    /// The port's calibrated CD ceiling remains one half of hardware gain;
    /// the original ten-step `bgmvolume` slider scales within that range.
    #[optimize(size)]
    fn set_volume(&mut self, level: u8) {
        let level = level.min(VOLUME_STEPS);
        if level == self.volume {
            return;
        }
        self.volume = level;
        let gain = psx_spu::CdVolume::linear(u16::from(level), u16::from(VOLUME_STEPS) * 2);
        psx_spu::set_cd_volume(gain, gain);
    }

    /// Turn the music on or off. Off pauses the drive rather than only
    /// declining to start it again, or the current song would play on under a
    /// menu that claims it is off.
    #[optimize(size)]
    #[inline(never)]
    fn set_enabled(&mut self, on: bool, tick: u32) {
        self.enabled = on;
        if on {
            self.begin_track(tick);
        } else {
            cdrom::try_pause(SPINS);
            self.starter = CddaStarter::new().with_spins(SPINS);
        }
    }

    /// Arm the start handshake for the current track and raise the banner.
    #[optimize(size)]
    #[inline(never)]
    fn begin_track(&mut self, tick: u32) {
        self.starter = CddaStarter::new().with_spins(SPINS);
        self.starter.begin(tick);
        self.end.rearm();
        self.next_poll = tick.wrapping_add(POLL_TICKS);
        self.announced_at = Some(tick);
    }

    /// Hand the drive back before a level load. Pause rather than Stop: the
    /// SDK's own note is that Stop winds the motor down for one to two seconds
    /// during which data reads fail, while Pause leaves it spun up and is
    /// "the right handoff before gameplay code starts issuing data-read
    /// commands". The loader's `SectorReader::prepare` re-issues SetMode
    /// without MODE_CDDA, so the mode does not have to be unwound here, and a
    /// drive that will not acknowledge the pause is about to be driven by
    /// `SectorReader` anyway, so there is no fallback to pay for.
    #[optimize(size)]
    #[inline(never)]
    pub fn suspend_for_load(&mut self) {
        if !self.available || !self.enabled {
            return;
        }
        cdrom::try_pause_until_complete(SPINS);
        self.starter = CddaStarter::new().with_spins(SPINS);
        self.announced_at = None;
    }

    /// Take the drive back after a level load, one song further on: a new level
    /// is the natural place for a track change, and the handshake has to be
    /// re-run regardless because the reader dropped MODE_CDDA.
    #[optimize(size)]
    #[inline(never)]
    pub fn resume_after_load(&mut self, tick: u32) {
        if !self.available || !self.enabled {
            return;
        }
        self.index = (self.index + 1) % TRACK_COUNT;
        self.begin_track(tick);
    }

    /// One tick. Drives the start handshake, and once playing, watches for the
    /// song ending so the next one can follow.
    #[optimize(size)]
    #[inline(never)]
    pub fn update(&mut self, tick: u32) {
        if !self.probed {
            if tick < PROBE_TICK {
                return;
            }
            self.probed = true;
            self.available = disc_has_menu_tracks();
            if self.available && self.enabled {
                self.begin_track(tick);
            }
            return;
        }
        if !self.available || !self.enabled {
            return;
        }

        // The handshake first: the drive tolerates command-then-status-drain,
        // and the launcher's console runs showed the reverse order wedging it.
        // The track is absolute, so the loader's base is pre-subtracted here to
        // cancel the shift `try_play_track` applies (see the module note).
        let absolute = FIRST_TRACK + self.index;
        self.starter.tick(
            tick,
            absolute.wrapping_sub(psx_io::disc_base::cdda_track_base()),
        );

        if self.starter.started() && tick.wrapping_sub(self.next_poll) < u32::MAX / 2 {
            self.next_poll = tick.wrapping_add(POLL_TICKS);
            let status = cdrom::try_get_stat(SPINS).and_then(|r| r.bytes().first().copied());
            if self.end.poll(status) {
                self.index = (self.index + 1) % TRACK_COUNT;
                self.begin_track(tick);
            }
        }
    }
}

/// Ask the drive how many tracks the disc has, and decide whether the menu
/// songs can be among them.
#[optimize(size)]
#[inline(never)]
fn disc_has_menu_tracks() -> bool {
    // Response is [status, first BCD, last BCD].
    let Some(response) = cdrom::try_command(GET_TN, &[], SPINS) else {
        return false;
    };
    let bytes = response.bytes();
    if bytes.len() < 3 {
        return false;
    }
    cdrom::bcd_to_bin(bytes[2]) >= TRACKS_NEEDED
}
