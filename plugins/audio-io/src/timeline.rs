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
//!                   (run start: audible ns, transport sample)
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
//! A *run* is a stretch of continuous playback: it starts when the transport
//! starts or seeks, and ends when it stops. Within a run, transport position is
//! a straight line in time, so knowing where the run started (in samples and in
//! audible nanoseconds) places any instant on the song. The output callback is
//! the only writer; readers use a seqlock and retry.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

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

/// The output side of the shared clock: the current run, published once per
/// block by the output callback.
#[derive(Debug)]
pub struct PlaybackTimeline {
    /// Odd while the writer is mid-update.
    seq: AtomicU64,
    /// Bumped every time a new run starts.
    run: AtomicU64,
    /// Transport sample the run started at.
    run_start_sample: AtomicU64,
    /// When that sample was (predicted to be) audible, in [`now_ns`] terms.
    run_start_ns: AtomicU64,
    /// When the run ended, or `u64::MAX` while it is still playing.
    run_end_ns: AtomicU64,
    /// The engine rate the run was rendered at, as `f64` bits.
    rate_bits: AtomicU64,
    playing: AtomicBool,
    /// Where the next block is expected to start if nothing seeks. Writer-only
    /// state, kept here so the writer itself needs no `&mut`.
    expected_next: AtomicU64,
}

/// One consistent read of the current run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackRun {
    pub run: u64,
    pub start_sample: u64,
    pub start_ns: u64,
    /// `u64::MAX` while playing.
    pub end_ns: u64,
    pub sample_rate: f64,
    pub playing: bool,
}

impl PlaybackRun {
    /// The transport position (fractional engine samples) that was audible at
    /// `at_ns`, extrapolating along this run. Can be before the run started or
    /// negative, which is the honest answer for audio captured before Play.
    pub fn sample_at(&self, at_ns: u64) -> f64 {
        let delta_ns = at_ns as f64 - self.start_ns as f64;
        self.start_sample as f64 + delta_ns * self.sample_rate / 1e9
    }

    /// Whether this run was playing at any point in `[from_ns, to_ns]`.
    pub fn overlaps(&self, from_ns: u64, to_ns: u64) -> bool {
        self.run > 0 && self.start_ns <= to_ns && self.end_ns >= from_ns
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
            run: AtomicU64::new(0),
            run_start_sample: AtomicU64::new(0),
            run_start_ns: AtomicU64::new(0),
            run_end_ns: AtomicU64::new(0),
            rate_bits: AtomicU64::new(0),
            playing: AtomicBool::new(false),
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
    /// `frames` how many were rendered while playing, and `audible_ns` when the
    /// first frame reaches the DAC.
    pub fn publish(
        &self,
        playing: bool,
        first_sample: u64,
        frames: u64,
        audible_ns: u64,
        sample_rate: f64,
    ) {
        let was_playing = self.playing.load(Ordering::Relaxed);
        if !playing {
            if was_playing {
                self.write(|timeline| {
                    timeline.run_end_ns.store(audible_ns, Ordering::Relaxed);
                    timeline.playing.store(false, Ordering::Relaxed);
                });
                self.expected_next.store(u64::MAX, Ordering::Relaxed);
            }
            return;
        }
        let continuous = was_playing && self.expected_next.load(Ordering::Relaxed) == first_sample;
        if !continuous {
            self.write(|timeline| {
                timeline.run.fetch_add(1, Ordering::Relaxed);
                timeline
                    .run_start_sample
                    .store(first_sample, Ordering::Relaxed);
                timeline.run_start_ns.store(audible_ns, Ordering::Relaxed);
                timeline.run_end_ns.store(u64::MAX, Ordering::Relaxed);
                timeline
                    .rate_bits
                    .store(sample_rate.to_bits(), Ordering::Relaxed);
                timeline.playing.store(true, Ordering::Relaxed);
            });
        }
        self.expected_next
            .store(first_sample.saturating_add(frames), Ordering::Relaxed);
    }

    fn write(&self, update: impl FnOnce(&Self)) {
        self.seq.fetch_add(1, Ordering::AcqRel);
        update(self);
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// A consistent copy of the current run. Non-RT readers only.
    pub fn read(&self) -> PlaybackRun {
        loop {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let run = PlaybackRun {
                run: self.run.load(Ordering::Relaxed),
                start_sample: self.run_start_sample.load(Ordering::Relaxed),
                start_ns: self.run_start_ns.load(Ordering::Relaxed),
                end_ns: self.run_end_ns.load(Ordering::Relaxed),
                sample_rate: f64::from_bits(self.rate_bits.load(Ordering::Relaxed)),
                playing: self.playing.load(Ordering::Relaxed),
            };
            std::sync::atomic::fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return run;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_starts_at_the_first_playing_block_and_stays_while_continuous() {
        let timeline = PlaybackTimeline::new();
        timeline.publish(false, 0, 0, 1_000, 48_000.0);
        assert_eq!(timeline.read().run, 0);
        timeline.publish(true, 4_800, 480, 2_000_000, 48_000.0);
        let first = timeline.read();
        assert_eq!(first.run, 1);
        assert_eq!(first.start_sample, 4_800);
        assert_eq!(first.start_ns, 2_000_000);
        assert!(first.playing);
        timeline.publish(true, 5_280, 480, 12_000_000, 48_000.0);
        assert_eq!(timeline.read(), first, "a continuous block is the same run");
    }

    #[test]
    fn a_seek_while_playing_starts_a_new_run() {
        let timeline = PlaybackTimeline::new();
        timeline.publish(true, 0, 480, 0, 48_000.0);
        timeline.publish(true, 96_000, 480, 10_000_000, 48_000.0);
        let run = timeline.read();
        assert_eq!(run.run, 2);
        assert_eq!(run.start_sample, 96_000);
    }

    #[test]
    fn stopping_closes_the_run_and_keeps_its_origin() {
        let timeline = PlaybackTimeline::new();
        timeline.publish(true, 0, 480, 5_000, 48_000.0);
        timeline.publish(false, 480, 0, 15_000, 48_000.0);
        let run = timeline.read();
        assert!(!run.playing);
        assert_eq!(run.end_ns, 15_000);
        assert_eq!(run.start_ns, 5_000);
        assert!(run.overlaps(0, 6_000));
        assert!(!run.overlaps(16_000, 20_000));
    }

    #[test]
    fn a_position_is_extrapolated_along_the_run_in_both_directions() {
        let run = PlaybackRun {
            run: 1,
            start_sample: 48_000,
            start_ns: 1_000_000_000,
            end_ns: u64::MAX,
            sample_rate: 48_000.0,
            playing: true,
        };
        assert_eq!(run.sample_at(1_000_000_000), 48_000.0);
        assert_eq!(run.sample_at(1_500_000_000), 72_000.0);
        assert_eq!(run.sample_at(990_000_000), 47_520.0, "10 ms before the run");
    }

    #[test]
    fn the_clock_is_monotonic() {
        init_clock();
        let a = now_ns();
        let b = now_ns();
        assert!(b >= a);
    }
}
