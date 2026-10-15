// SPDX-License-Identifier: GPL-2.0-or-later
//! The schedule behind Quake's sporadic dark-ambient music.
//!
//! The port does not keep a soundtrack running. After a map starts there is a
//! stretch of silence, then one loop fades in, plays once to three times, fades
//! out, and the silence returns. This module owns only the timing and the
//! choices; it asks for nothing from the drive. The game's `music` module turns
//! each [`Step`] into CD commands and SPU volume writes, and calls
//! [`Schedule::halt`] before every map load and [`Schedule::restart`] after, so
//! no schedule ever straddles a data read.
//!
//! All times are 60 Hz ticks. The schedule draws from its own small generator
//! (the same linear congruential family the monsters and weapons use, none of
//! which is shared), so choosing a loop can never advance or read a gameplay
//! stream: replays of a recorded route stay identical with the music on.

/// Ticks per second the durations below are written in.
pub const TICKS_PER_SECOND: u32 = 60;

/// Silence after a map starts, in seconds: lowest and highest.
pub const FIRST_WAIT_SECONDS: (u32, u32) = (30, 90);
/// Silence between two appearances of music, in seconds.
pub const REST_SECONDS: (u32, u32) = (60, 180);
/// How long a loop takes to rise to full level.
pub const FADE_IN_TICKS: u32 = 3 * TICKS_PER_SECOND;
/// How long it takes to fall back to silence. Longer than the rise: the tail
/// of a dark ambience is what the player should barely notice leaving.
pub const FADE_OUT_TICKS: u32 = 6 * TICKS_PER_SECOND;
/// Most times one loop is heard in a row.
pub const MAX_PLAYS: u32 = 3;
/// Fade level at full volume. Levels run from 0 to this.
pub const FADE_MAX: u16 = 256;

/// What the game's music module should do after one [`Schedule::update`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Step {
    /// Nothing changed.
    Idle,
    /// Begin this loop from the top, at fade level 0.
    Start(u8),
    /// Set the fade to this level (`0..=FADE_MAX`).
    Fade(u16),
    /// The last fade-out finished: stop the drive.
    Stop,
}

/// A small linear congruential generator, private to the schedule.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AmbientRandom(u32);

impl AmbientRandom {
    pub const fn new(seed: u32) -> Self {
        Self(seed)
    }

    /// The next value, from the generator's better-mixed high bits.
    pub fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 8
    }

    /// A value in `low..=high`.
    pub fn range(&mut self, low: u32, high: u32) -> u32 {
        debug_assert!(low <= high);
        low + self.next() % (high - low + 1)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// No music wanted: the music is off, or a map is loading.
    Off,
    /// Waiting for `until`.
    Silent { until: u32 },
    /// A loop is up; `since` is the tick it started, `stop_at` the tick its
    /// fade-out reaches silence.
    Playing { since: u32, stop_at: u32 },
}

/// The sporadic-music state machine.
#[derive(Copy, Clone, Debug)]
pub struct Schedule {
    random: AmbientRandom,
    phase: Phase,
    /// Length of every loop, in ticks. The game derives it from the encoder's
    /// song table, so a loop is timed by the audio it actually carries.
    song_ticks: &'static [u32],
    previous: Option<u8>,
    current: u8,
    level: u16,
    first_wait: (u32, u32),
    rest_wait: (u32, u32),
}

impl Schedule {
    /// A schedule over `song_ticks.len()` loops, off until [`Self::restart`].
    pub const fn new(seed: u32, song_ticks: &'static [u32]) -> Self {
        Self {
            random: AmbientRandom::new(seed),
            phase: Phase::Off,
            song_ticks,
            previous: None,
            current: 0,
            level: 0,
            first_wait: FIRST_WAIT_SECONDS,
            rest_wait: REST_SECONDS,
        }
    }

    /// The same schedule with other silences (seconds, lowest and highest).
    /// Only for the headless gate image, which has to reach a loop within a
    /// few seconds of a map start; the shipping game uses [`Self::new`].
    pub const fn with_waits(mut self, first: (u32, u32), rest: (u32, u32)) -> Self {
        self.first_wait = first;
        self.rest_wait = rest;
        self
    }

    /// Replace the generator's state. Called once, before the first
    /// [`Self::restart`], with something that differs between sessions.
    pub fn reseed(&mut self, seed: u32) {
        self.random = AmbientRandom::new(seed);
    }

    /// The loop being played, while one is.
    pub fn playing(&self) -> Option<u8> {
        matches!(self.phase, Phase::Playing { .. }).then_some(self.current)
    }

    /// Fade level last asked for.
    pub fn level(&self) -> u16 {
        self.level
    }

    /// Whether a loop is up (rising, held or falling).
    pub fn is_playing(&self) -> bool {
        matches!(self.phase, Phase::Playing { .. })
    }

    /// Whether the schedule wants to run at all.
    pub fn is_off(&self) -> bool {
        matches!(self.phase, Phase::Off)
    }

    /// Stop scheduling, forgetting any loop in progress. The caller silences
    /// the drive itself; this only makes the state agree.
    pub fn halt(&mut self) {
        self.phase = Phase::Off;
        self.level = 0;
    }

    /// Begin a fresh schedule at tick `now`: a map has just started, or the
    /// music was just switched on. The first loop is a long way off.
    pub fn restart(&mut self, now: u32) {
        self.level = 0;
        self.phase = Phase::Silent {
            until: now.wrapping_add(self.wait(self.first_wait)),
        };
    }

    fn wait(&mut self, seconds: (u32, u32)) -> u32 {
        self.random.range(seconds.0, seconds.1) * TICKS_PER_SECOND
    }

    /// A loop other than the one heard last. With a single loop there is no
    /// other to pick.
    fn pick(&mut self) -> u8 {
        let count = self.song_ticks.len() as u32;
        let song = match self.previous {
            Some(last) if count > 1 => {
                let draw = self.random.range(0, count - 2) as u8;
                if draw >= last {
                    draw + 1
                } else {
                    draw
                }
            }
            _ => self.random.range(0, count - 1) as u8,
        };
        self.previous = Some(song);
        song
    }

    /// Advance to tick `now`.
    pub fn update(&mut self, now: u32) -> Step {
        match self.phase {
            Phase::Off => Step::Idle,
            Phase::Silent { until } => {
                if now.wrapping_sub(until) >= u32::MAX / 2 || self.song_ticks.is_empty() {
                    return Step::Idle;
                }
                let song = self.pick();
                let plays = self.random.range(1, MAX_PLAYS);
                let length = self.song_ticks[song as usize].max(FADE_IN_TICKS + FADE_OUT_TICKS);
                self.current = song;
                self.level = 0;
                self.phase = Phase::Playing {
                    since: now,
                    stop_at: now.wrapping_add(length * plays),
                };
                Step::Start(song)
            }
            Phase::Playing { since, stop_at } => {
                if now.wrapping_sub(stop_at) < u32::MAX / 2 {
                    self.level = 0;
                    self.phase = Phase::Silent {
                        until: now.wrapping_add(self.wait(self.rest_wait)),
                    };
                    return Step::Stop;
                }
                let rising = (now.wrapping_sub(since)).min(FADE_IN_TICKS);
                let falling = (stop_at.wrapping_sub(now)).min(FADE_OUT_TICKS);
                let rise = rising * u32::from(FADE_MAX) / FADE_IN_TICKS;
                let fall = falling * u32::from(FADE_MAX) / FADE_OUT_TICKS;
                let level = rise.min(fall) as u16;
                if level == self.level {
                    Step::Idle
                } else {
                    self.level = level;
                    Step::Fade(level)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static SONGS: [u32; 8] = [2400, 2700, 2760, 2400, 2640, 3420, 2520, 3120];

    fn run(schedule: &mut Schedule, from: u32, to: u32) -> std::vec::Vec<(u32, Step)> {
        let mut steps = std::vec::Vec::new();
        for now in from..to {
            let step = schedule.update(now);
            if step != Step::Idle {
                steps.push((now, step));
            }
        }
        steps
    }

    extern crate std;

    #[test]
    fn first_loop_comes_after_thirty_to_ninety_seconds() {
        for seed in 0..200 {
            let mut schedule = Schedule::new(seed, &SONGS);
            schedule.restart(1000);
            let steps = run(&mut schedule, 1000, 1000 + 100 * TICKS_PER_SECOND);
            let (at, step) = steps[0];
            assert!(matches!(step, Step::Start(_)), "seed {seed}");
            let waited = at - 1000;
            assert!(
                (30 * TICKS_PER_SECOND..=90 * TICKS_PER_SECOND).contains(&waited),
                "seed {seed} waited {waited}"
            );
        }
    }

    #[test]
    fn a_cycle_fades_in_holds_fades_out_and_rests() {
        let mut schedule = Schedule::new(7, &SONGS);
        schedule.restart(0);
        let steps = run(&mut schedule, 0, 6 * 60 * TICKS_PER_SECOND);
        let starts: std::vec::Vec<_> = steps
            .iter()
            .filter(|(_, s)| matches!(s, Step::Start(_)))
            .collect();
        let stops: std::vec::Vec<_> = steps.iter().filter(|(_, s)| *s == Step::Stop).collect();
        assert!(starts.len() >= 2 && stops.len() >= 2);
        let (start_at, Step::Start(song)) = *starts[0] else {
            unreachable!()
        };
        let stop_at = stops[0].0;
        // Heard one to three times, the stop falling on the pass boundary.
        let length = SONGS[song as usize];
        let heard = stop_at - start_at;
        assert!(
            (1..=MAX_PLAYS).any(|n| heard == n * length),
            "heard {heard} of a {length} tick loop"
        );
        // Rest between the stop and the next start is 60 to 180 s.
        let rest = starts[1].0 - stop_at;
        assert!(
            (60 * TICKS_PER_SECOND..=180 * TICKS_PER_SECOND).contains(&rest),
            "rest {rest}"
        );
        // Levels rise to full and are never out of range, reaching 0 at the stop.
        let levels: std::vec::Vec<u16> = steps
            .iter()
            .filter(|(at, _)| (start_at..=stop_at).contains(at))
            .filter_map(|(_, s)| {
                if let Step::Fade(l) = s {
                    Some(*l)
                } else {
                    None
                }
            })
            .collect();
        assert!(levels.contains(&FADE_MAX));
        assert!(levels.iter().all(|l| *l <= FADE_MAX));
    }

    #[test]
    fn fade_in_and_out_take_their_set_lengths() {
        let mut schedule = Schedule::new(3, &SONGS);
        schedule.restart(0);
        let steps = run(&mut schedule, 0, 4 * 60 * TICKS_PER_SECOND);
        let (start_at, Step::Start(song)) = steps
            .iter()
            .copied()
            .find(|(_, s)| matches!(s, Step::Start(_)))
            .unwrap()
        else {
            unreachable!()
        };
        let first_full = steps
            .iter()
            .find(|(at, s)| *at > start_at && *s == Step::Fade(FADE_MAX))
            .unwrap()
            .0;
        assert_eq!(first_full - start_at, FADE_IN_TICKS);
        let stop_at = steps
            .iter()
            .find(|(at, s)| *at > start_at && *s == Step::Stop)
            .unwrap()
            .0;
        let last_full = steps
            .iter()
            .filter(|(at, s)| *at < stop_at && *s == Step::Fade(FADE_MAX))
            .last()
            .map(|(at, _)| *at)
            .unwrap_or(first_full);
        // The level falls from full over the last six seconds of the last pass.
        let falling: std::vec::Vec<u16> = steps
            .iter()
            .filter(|(at, _)| *at > stop_at - FADE_OUT_TICKS && *at < stop_at)
            .filter_map(|(_, s)| {
                if let Step::Fade(l) = s {
                    Some(*l)
                } else {
                    None
                }
            })
            .collect();
        assert!(falling.windows(2).all(|w| w[0] > w[1]), "{falling:?}");
        assert!(last_full <= stop_at - FADE_OUT_TICKS + 1);
        let _ = song;
    }

    #[test]
    fn the_same_loop_never_plays_twice_in_a_row() {
        for seed in 0..50 {
            let mut schedule = Schedule::new(seed, &SONGS);
            schedule.restart(0);
            let steps = run(&mut schedule, 0, 60 * 60 * TICKS_PER_SECOND);
            let songs: std::vec::Vec<u8> = steps
                .iter()
                .filter_map(|(_, s)| {
                    if let Step::Start(n) = s {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .collect();
            assert!(songs.len() > 8);
            assert!(songs.windows(2).all(|w| w[0] != w[1]), "seed {seed}");
        }
    }

    #[test]
    fn every_loop_gets_picked_eventually() {
        let mut schedule = Schedule::new(11, &SONGS);
        schedule.restart(0);
        let steps = run(&mut schedule, 0, 24 * 60 * 60 * TICKS_PER_SECOND);
        let mut seen = [false; 8];
        for (_, s) in steps {
            if let Step::Start(n) = s {
                seen[n as usize] = true;
            }
        }
        assert!(seen.iter().all(|s| *s));
    }

    #[test]
    fn halting_silences_the_schedule_until_restarted() {
        let mut schedule = Schedule::new(5, &SONGS);
        schedule.restart(0);
        let steps = run(&mut schedule, 0, 100 * TICKS_PER_SECOND);
        assert!(steps.iter().any(|(_, s)| matches!(s, Step::Start(_))));
        assert!(schedule.is_playing());
        schedule.halt();
        assert!(schedule.is_off() && !schedule.is_playing());
        assert_eq!(schedule.level(), 0);
        assert!(run(
            &mut schedule,
            100 * TICKS_PER_SECOND,
            5000 * TICKS_PER_SECOND
        )
        .is_empty());
        // After the load the wait starts over from the new tick.
        let resume = 6000 * TICKS_PER_SECOND;
        schedule.restart(resume);
        let steps = run(&mut schedule, resume, resume + 100 * TICKS_PER_SECOND);
        let waited = steps[0].0 - resume;
        assert!(waited >= 30 * TICKS_PER_SECOND);
    }

    #[test]
    fn a_single_loop_still_repeats() {
        static ONE: [u32; 1] = [3000];
        let mut schedule = Schedule::new(2, &ONE);
        schedule.restart(0);
        let steps = run(&mut schedule, 0, 30 * 60 * TICKS_PER_SECOND);
        let starts = steps.iter().filter(|(_, s)| *s == Step::Start(0)).count();
        assert!(starts >= 2);
    }

    #[test]
    fn the_generator_is_private_state() {
        // Two schedules with one seed agree; nothing else is consulted.
        let mut a = Schedule::new(99, &SONGS);
        let mut b = Schedule::new(99, &SONGS);
        a.restart(10);
        b.restart(10);
        assert_eq!(run(&mut a, 10, 20000), run(&mut b, 10, 20000));
    }
}
