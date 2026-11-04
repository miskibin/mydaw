//! Where a recorded sample belongs on the song: one clock both streams agree on.
//!
//! ```text
//!   output callback (RT)                     input callback (RT)
//!   --------------------                     -------------------
//!   now_ns() + (playback - callback)         now_ns() - (callback - capture)
//!     = when this block's first frame          = when this block's first frame
//!       reaches the speaker                      left the microphone
//!          |                                        |
//!          '--> PlaybackTimeline::publish            '--> take anchor (InputShared)
//!                   (runs: audible ns <-> transport sample)
//!                                   \              /
//!                                    v            v
//!                         RecordService::stop -> RecordedTake::start_sample
//! ```
//!
//! # Why device timestamps, not "the take starts when Play is pressed"
//! A singer hears the beat one output latency after the engine renders it, and
//! the microphone hands us their voice one input latency after they sing. Both
//! are 5-40 ms on a shared-mode Windows device, and they add up: a take
//! placed at "the moment Play was pressed" lands audibly late, which is exactly
//! the FL Studio failure its *latency compensation* option exists to fix.
//!
//! cpal reports, per callback, when the block was captured (input) and when it
//! will be played (output). Those instants are only comparable *within* one
//! stream -- an ASIO output and a WASAPI input do not share an origin -- so each
//! callback turns its own stream's latency into a difference and anchors it to
//! one process-wide monotonic clock ([`now_ns`]). That clock is
//! `QueryPerformanceCounter` on Windows: no allocation, no lock, no syscall
//! that can block, which is what makes it legal in both callbacks.
//!
//! # Runs
//! A *run* is a stretch of continuous playback in one mode: it starts when the
//! transport starts, seeks or changes mode, and ends when it stops. Within a
//! run, transport position is a straight line in time. The timeline keeps the
//! last [`RUN_HISTORY`] runs, each with its first block and its latest block,
//! so a take is placed against the run that was actually playing when it
//! began -- not whatever was playing when it ended -- and extrapolated from the
//! nearest measured point, which keeps clock drift out of a late punch-in. The
//! output callback is the only writer; readers use a seqlock and retry.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Runs remembered. A take spans a handful at most (Play, a seek or two).
pub const RUN_HISTORY: usize = 8;

/// The origin every timestamp in this crate is measured from.
fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Prime the clock's origin. Called from non-RT constructors so the first
/// callback never pays for the `OnceLock` initialisation.
pub fn init_clock() {
    let _ = epoch();
}

/// Nanoseconds since the process-wide origin. RT-safe.
#[inline]
pub fn now_ns() -> u64 {
    duration_ns(Instant::now().saturating_duration_since(epoch()))
}

/// A `Duration` as nanoseconds, saturating.
#[inline]
pub fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// One remembered run, as atomics.
#[derive(Debug)]
struct RunSlot {
    run: AtomicU64,
    start_sample: AtomicU64,
    start_ns: AtomicU64,
    last_sample: AtomicU64,
    last_ns: AtomicU64,
    end_ns: AtomicU64,
    rate_bits: AtomicU64,
    song: AtomicBool,
}

impl RunSlot {
    const fn new() -> Self {
        Self {
            run: AtomicU64::new(0),
            start_sample: AtomicU64::new(0),
            start_ns: AtomicU64::new(0),
            last_sample: AtomicU64::new(0),
            last_ns: AtomicU64::new(0),
            end_ns: AtomicU64::new(0),
            rate_bits: AtomicU64::new(0),
            song: AtomicBool::new(false),
        }
    }

    fn read(&self) -> PlaybackRun {
        PlaybackRun {
            run: self.run.load(Ordering::Relaxed),
            start_sample: self.start_sample.load(Ordering::Relaxed),
            start_ns: self.start_ns.load(Ordering::Relaxed),
            last_sample: self.last_sample.load(Ordering::Relaxed),
            last_ns: self.last_ns.load(Ordering::Relaxed),
            end_ns: self.end_ns.load(Ordering::Relaxed),
            sample_rate: f64::from_bits(self.rate_bits.load(Ordering::Relaxed)),
            song: self.song.load(Ordering::Relaxed),
        }
    }
}

/// The output side of the shared clock, published once per block by the
/// output callback.
#[derive(Debug)]
pub struct PlaybackTimeline {
    /// Odd while the writer is mid-update.
    seq: AtomicU64,
    runs: [RunSlot; RUN_HISTORY],
    /// Index of the current (newest) run in `runs`.
    current: AtomicUsize,
    /// Runs ever started.
    count: AtomicU64,
    playing: AtomicBool,
    /// Mode of the current run: 1 = song, 0 = pattern.
    mode: AtomicU8,
    /// Where the next block is expected to start if nothing seeks. Writer-only.
    expected_next: AtomicU64,
}

/// One consistent read of a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackRun {
    /// `0` = never started.
    pub run: u64,
    pub start_sample: u64,
    /// When `start_sample` was audible.
    pub start_ns: u64,
    /// The latest block's first sample, and when it was audible.
    pub last_sample: u64,
    pub last_ns: u64,
    /// `u64::MAX` while playing.
    pub end_ns: u64,
    pub sample_rate: f64,
    /// Whether the run played the song (rather than a pattern loop).
    pub song: bool,
}

impl PlaybackRun {
    /// The transport position (fractional samples at [`Self::sample_rate`])
    /// that was audible at `at_ns`, extrapolated from whichever measured point
    /// of the run is nearer. Can be before the run started, or negative, which
    /// is the honest answer for audio captured before Play.
    pub fn sample_at(&self, at_ns: u64) -> f64 {
        let (sample, ns) = if at_ns.abs_diff(self.last_ns) < at_ns.abs_diff(self.start_ns) {
            (self.last_sample, self.last_ns)
        } else {
            (self.start_sample, self.start_ns)
        };
        let delta_ns = at_ns as f64 - ns as f64;
        sample as f64 + delta_ns * self.sample_rate / 1e9
    }

    /// Whether this run was playing at `at_ns`.
    pub fn contains(&self, at_ns: u64) -> bool {
        self.run > 0 && self.start_ns <= at_ns && at_ns <= self.end_ns
    }
}

impl Default for PlaybackTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackTimeline {
    /// No run yet.
    pub const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            runs: [const { RunSlot::new() }; RUN_HISTORY],
            current: AtomicUsize::new(0),
            count: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            mode: AtomicU8::new(0),
            expected_next: AtomicU64::new(u64::MAX),
        }
    }

    /// The timeline the live output stream publishes to.
    pub fn global() -> &'static PlaybackTimeline {
        static GLOBAL: PlaybackTimeline = PlaybackTimeline::new();
        &GLOBAL
    }

    /// Publish one rendered block. **RT**: atomics only.
    ///
    /// `first_sample` is the transport position of the block's first frame,
    /// `frames` how many were rendered while playing, `audible_ns` when the
    /// first frame reaches the DAC, and `song` whether the transport is in song
    /// mode.
    pub fn publish(
        &self,
        playing: bool,
        song: bool,
        first_sample: u64,
        frames: u64,
        audible_ns: u64,
        sample_rate: f64,
    ) {
        let was_playing = self.playing.load(Ordering::Relaxed);
        let current = self.current.load(Ordering::Relaxed);
        if !playing {
            if was_playing {
                self.write(|timeline| {
                    timeline.runs[current]
                        .end_ns
                        .store(audible_ns, Ordering::Relaxed);
                    timeline.playing.store(false, Ordering::Relaxed);
                });
                self.expected_next.store(u64::MAX, Ordering::Relaxed);
            }
            return;
        }
        let continuous = was_playing
            && self.expected_next.load(Ordering::Relaxed) == first_sample
            && (self.mode.load(Ordering::Relaxed) == 1) == song;
        self.write(|timeline| {
            let index = if continuous {
                current
            } else {
                if was_playing {
                    timeline.runs[current]
                        .end_ns
                        .store(audible_ns, Ordering::Relaxed);
                }
                let next = if timeline.count.load(Ordering::Relaxed) == 0 {
                    0
                } else {
                    (current + 1) % RUN_HISTORY
                };
                let number = timeline.count.fetch_add(1, Ordering::Relaxed) + 1;
                let slot = &timeline.runs[next];
                slot.run.store(number, Ordering::Relaxed);
                slot.start_sample.store(first_sample, Ordering::Relaxed);
                slot.start_ns.store(audible_ns, Ordering::Relaxed);
                slot.end_ns.store(u64::MAX, Ordering::Relaxed);
                slot.rate_bits
                    .store(sample_rate.to_bits(), Ordering::Relaxed);
                slot.song.store(song, Ordering::Relaxed);
                timeline.current.store(next, Ordering::Relaxed);
                timeline.mode.store(u8::from(song), Ordering::Relaxed);
                timeline.playing.store(true, Ordering::Relaxed);
                next
            };
            let slot = &timeline.runs[index];
            slot.last_sample.store(first_sample, Ordering::Relaxed);
            slot.last_ns.store(audible_ns, Ordering::Relaxed);
        });
        self.expected_next
            .store(first_sample.saturating_add(frames), Ordering::Relaxed);
    }

    fn write(&self, update: impl FnOnce(&Self)) {
        self.seq.fetch_add(1, Ordering::AcqRel);
        update(self);
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// Every remembered run, newest first. Non-RT readers only.
    pub fn runs(&self) -> Vec<PlaybackRun> {
        loop {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let current = self.current.load(Ordering::Relaxed);
            let runs: Vec<PlaybackRun> = (0..RUN_HISTORY)
                .map(|back| self.runs[(current + RUN_HISTORY - back) % RUN_HISTORY].read())
                .filter(|run| run.run > 0)
                .collect();
            std::sync::atomic::fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return runs;
            }
        }
    }

    /// The newest run, or a never-started one.
    pub fn read(&self) -> PlaybackRun {
        self.runs().into_iter().next().unwrap_or(PlaybackRun {
            run: 0,
            start_sample: 0,
            start_ns: 0,
            last_sample: 0,
            last_ns: 0,
            end_ns: 0,
            sample_rate: 0.0,
            song: false,
        })
    }

    /// The run a take captured from `captured_ns` until `ended_ns` belongs to:
    /// the one playing when it began, or -- for a take started a moment before
    /// Play -- the first one that began during it.
    pub fn run_for_take(&self, captured_ns: u64, ended_ns: u64) -> Option<PlaybackRun> {
        let runs = self.runs();
        if let Some(run) = runs.iter().find(|run| run.contains(captured_ns)) {
            return Some(*run);
        }
        runs.iter()
            .rev()
            .find(|run| run.start_ns > captured_ns && run.start_ns <= ended_ns)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_starts_at_the_first_playing_block_and_stays_while_continuous() {
        let timeline = PlaybackTimeline::new();
        timeline.publish(false, true, 0, 0, 1_000, 48_000.0);
        assert_eq!(timeline.read().run, 0);
        timeline.publish(true, true, 4_800, 480, 2_000_000, 48_000.0);
        let first = timeline.read();
        assert_eq!(first.run, 1);
        assert_eq!(first.start_sample, 4_800);
        assert_eq!(first.start_ns, 2_000_000);
        timeline.publish(true, true, 5_280, 480, 12_000_000, 48_000.0);
        let later = timeline.read();
        assert_eq!(later.run, 1, "a continuous block is the same run");
        assert_eq!(later.last_sample, 5_280);
        assert_eq!(later.last_ns, 12_000_000);
    }

    #[test]
    fn a_seek_or_a_mode_switch_while_playing_starts_a_new_run() {
        let timeline = PlaybackTimeline::new();
        timeline.publish(true, true, 0, 480, 0, 48_000.0);
        timeline.publish(true, true, 96_000, 480, 10_000_000, 48_000.0);
        assert_eq!(timeline.read().run, 2);
        assert_eq!(timeline.read().start_sample, 96_000);
        timeline.publish(true, false, 96_480, 480, 20_000_000, 48_000.0);
        let run = timeline.read();
        assert_eq!(run.run, 3);
        assert!(!run.song);
    }

    #[test]
    fn a_take_is_placed_against_the_run_playing_when_it_began() {
        let timeline = PlaybackTimeline::new();
        // Song from 0 at t = 1 s, then a seek to 480 000 at t = 3 s.
        timeline.publish(true, true, 0, 480, 1_000_000_000, 48_000.0);
        timeline.publish(true, true, 480_000, 480, 3_000_000_000, 48_000.0);
        let run = timeline
            .run_for_take(2_000_000_000, 4_000_000_000)
            .expect("a run was playing");
        assert_eq!(run.start_sample, 0);
        assert_eq!(run.sample_at(2_000_000_000), 48_000.0);
    }

    #[test]
    fn a_take_begun_just_before_play_belongs_to_the_run_that_followed() {
        let timeline = PlaybackTimeline::new();
        timeline.publish(true, true, 0, 480, 1_000_000_000, 48_000.0);
        let run = timeline.run_for_take(990_000_000, 5_000_000_000).unwrap();
        assert_eq!(run.sample_at(990_000_000), -480.0, "10 ms before the run");
        assert_eq!(
            timeline
                .run_for_take(5_000_000_000, 6_000_000_000)
                .map(|r| r.run),
            Some(1)
        );
    }

    #[test]
    fn a_take_over_a_stopped_transport_has_no_run() {
        let timeline = PlaybackTimeline::new();
        assert!(timeline.run_for_take(5, 9).is_none());
        timeline.publish(true, true, 0, 480, 0, 48_000.0);
        timeline.publish(false, true, 480, 0, 1_000, 48_000.0);
        assert!(timeline.run_for_take(5_000, 9_000).is_none());
    }

    #[test]
    fn a_late_punch_in_extrapolates_from_the_latest_block_not_the_run_start() {
        // The device clock runs 100 ppm fast: after 180 s the transport is
        // 864 samples ahead of nominal. The latest block knows that; the run
        // start does not.
        let run = PlaybackRun {
            run: 1,
            start_sample: 0,
            start_ns: 0,
            last_sample: 8_640_864,
            last_ns: 180_000_000_000,
            end_ns: u64::MAX,
            sample_rate: 48_000.0,
            song: true,
        };
        assert_eq!(run.sample_at(180_010_000_000), 8_641_344.0);
        assert_eq!(run.sample_at(10_000_000), 480.0);
    }

    #[test]
    fn old_runs_roll_off_after_the_history() {
        let timeline = PlaybackTimeline::new();
        for index in 0..(RUN_HISTORY as u64 + 3) {
            timeline.publish(
                true,
                true,
                index * 1_000_000,
                480,
                index * 1_000_000_000,
                48_000.0,
            );
        }
        let runs = timeline.runs();
        assert_eq!(runs.len(), RUN_HISTORY);
        assert_eq!(runs[0].run, RUN_HISTORY as u64 + 3, "newest first");
    }

    #[test]
    fn the_clock_is_monotonic() {
        init_clock();
        let a = now_ns();
        let b = now_ns();
        assert!(b >= a);
    }
}
