//! The real-time half of the debug tap: an armable, non-blocking sample sink.
//!
//! ```text
//!   audio thread (RT)                     capture thread (non-RT)
//!   -----------------                     -----------------------
//!   TapRtSide::push_block  ==rtrb f32==>  TapConsumer::drain_into
//!          ^                                     |
//!          '------- TapShared::armed <-----------'  (arm / disarm)
//! ```
//!
//! # RT cost
//! While disarmed, [`TapRtSide::push_block`] is *one relaxed atomic load and a
//! return* -- the branch predicts perfectly and nothing else is touched. That is
//! the whole reason the arming flag exists: an always-on tap would copy every
//! block of every session for a feature that is used for seconds at a time.
//!
//! While armed it writes `frames * 2` `f32`s into a pre-allocated ring through a
//! single `rtrb` chunk reservation: no allocation, no lock, no syscall, and no
//! blocking. If the ring is full the samples are counted in
//! [`TapShared::dropped`] and thrown away, exactly like a full meter queue -- a
//! gap in a debug capture is a nuisance, a stalled audio callback is a click.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use rtrb::{Consumer, Producer, RingBuffer};

/// Channel count of a capture. The engine is stereo (`mydaw_rt_core::CHANNELS`)
/// and the tap deliberately mirrors the *bus*, not the device: a 6-channel
/// output device still yields a 2-channel capture, because that is what the
/// graph produced.
pub const CHANNELS: usize = mydaw_rt_core::CHANNELS;

/// Default ring depth, in samples (not frames).
///
/// 65 536 samples = 32 768 stereo frames, about 0.68 s at 48 kHz. The capture
/// loop drains every millisecond, so steady-state occupancy is ~100 samples;
/// the rest is headroom for a scheduling hiccup on the draining thread.
pub const TAP_QUEUE_CAPACITY: usize = 1 << 16;

/// State shared between the audio thread and the capture thread.
///
/// Every field is an atomic touched with `Relaxed` ordering: the arming flag is
/// advisory (arming a block late is invisible), and the counters are statistics.
/// The samples themselves are ordered by the `rtrb` ring, which does its own
/// acquire/release.
#[derive(Debug)]
pub struct TapShared {
    armed: AtomicBool,
    dropped: AtomicU64,
    /// `f64` bits of the sample rate the audio thread is running at, or 0 when
    /// no stream has ever been opened. Written once per stream, on a non-RT
    /// thread.
    sample_rate_bits: AtomicU64,
}

impl Default for TapShared {
    fn default() -> Self {
        Self {
            armed: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            sample_rate_bits: AtomicU64::new(0),
        }
    }
}

impl TapShared {
    /// Whether the audio thread is currently copying samples.
    #[inline]
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Relaxed)
    }

    /// Start copying. Non-RT side only.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::Relaxed);
    }

    /// Stop copying. Non-RT side only.
    pub fn disarm(&self) {
        self.armed.store(false, Ordering::Relaxed);
    }

    /// Samples the audio thread had to throw away because the ring was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Zero the drop counter (done at the start of every capture).
    pub fn reset_dropped(&self) {
        self.dropped.store(0, Ordering::Relaxed);
    }

    /// Sample rate the audio thread last reported, or `0.0` if no stream has
    /// run.
    pub fn sample_rate(&self) -> f64 {
        f64::from_bits(self.sample_rate_bits.load(Ordering::Relaxed))
    }

    /// Record the sample rate of the stream being built. Non-RT.
    ///
    /// Non-finite and non-positive values are rejected rather than stored, so
    /// `sample_rate()` is either 0 (unknown) or a usable number.
    pub fn set_sample_rate(&self, sample_rate: f64) {
        let value = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            0.0
        };
        self.sample_rate_bits
            .store(value.to_bits(), Ordering::Relaxed);
    }

    fn record_dropped(&self, samples: u64) {
        self.dropped.fetch_add(samples, Ordering::Relaxed);
    }
}

/// The audio thread's end of the tap.
///
/// Handed over exactly once by [`crate::DebugTapHandoff`] and moved into the
/// cpal callback, in the same way `audio-io` claims the meter producer. It is
/// returned on stream teardown so a stop/start cycle keeps the tap alive.
pub struct TapRtSide {
    producer: Producer<f32>,
    shared: Arc<TapShared>,
}

impl TapRtSide {
    /// Shared flags and counters, for the non-RT side.
    pub fn shared(&self) -> &Arc<TapShared> {
        &self.shared
    }

    /// Tell the capture side what rate this stream runs at. **Non-RT**: call it
    /// while building the RT state, not from the callback.
    pub fn set_sample_rate(&self, sample_rate: f64) {
        self.shared.set_sample_rate(sample_rate);
    }

    /// Whether a capture is in progress. One relaxed load.
    #[inline]
    pub fn is_armed(&self) -> bool {
        self.shared.is_armed()
    }

    /// Samples dropped so far because the ring was full.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped()
    }

    /// Free slots in the ring, in samples. Test and diagnostic helper.
    pub fn free_slots(&self) -> usize {
        self.producer.slots()
    }

    /// Copy one block into the ring, interleaved `L R L R ...`.
    ///
    /// Only the first `min(left.len(), right.len())` frames are taken, so a
    /// caller that passes mismatched slices truncates rather than reading out of
    /// bounds.
    ///
    /// # RT safety
    /// Disarmed: one relaxed atomic load. Armed: one chunk reservation and a
    /// straight-line copy into memory allocated by [`debug_tap_channel`]. Never
    /// allocates, never blocks, never fails -- a full ring increments
    /// [`TapShared::dropped`] instead.
    ///
    /// Frames are kept whole: a block that only half fits contributes an
    /// integral number of frames, so the reader never loses channel alignment.
    #[inline]
    pub fn push_block(&mut self, left: &[f32], right: &[f32]) {
        if !self.shared.is_armed() {
            return;
        }
        let frames = left.len().min(right.len());
        if frames == 0 {
            return;
        }

        // Round the reservation down to whole frames; see the alignment note.
        let fit = (self.producer.slots() / CHANNELS).min(frames);
        if fit > 0 {
            if let Ok(chunk) = self.producer.write_chunk_uninit(fit * CHANNELS) {
                let interleaved = left[..fit]
                    .iter()
                    .zip(&right[..fit])
                    .flat_map(|(l, r)| [*l, *r]);
                chunk.fill_from_iter(interleaved);
            }
        }

        if fit < frames {
            self.shared
                .record_dropped(((frames - fit) * CHANNELS) as u64);
        }
    }
}

/// The capture thread's end of the tap.
pub struct TapConsumer {
    consumer: Consumer<f32>,
    shared: Arc<TapShared>,
}

impl TapConsumer {
    /// Shared flags and counters.
    pub fn shared(&self) -> &Arc<TapShared> {
        &self.shared
    }

    /// Samples currently waiting in the ring.
    pub fn available(&self) -> usize {
        self.consumer.slots()
    }

    /// Discard everything currently in the ring.
    ///
    /// Called before arming so a capture never begins with samples from whatever
    /// the engine was doing minutes ago.
    pub fn drain_all(&mut self) {
        loop {
            let available = self.consumer.slots();
            if available == 0 {
                return;
            }
            match self.consumer.read_chunk(available) {
                Ok(chunk) => chunk.commit_all(),
                Err(_) => return,
            }
        }
    }

    /// Move whatever is available into `out`, stopping at `limit` total samples.
    ///
    /// Returns how many samples were appended.
    pub fn drain_into(&mut self, out: &mut Vec<f32>, limit: usize) -> usize {
        let before = out.len();
        while out.len() < limit {
            let available = self.consumer.slots();
            if available == 0 {
                break;
            }
            let take = available.min(limit - out.len());
            let Ok(chunk) = self.consumer.read_chunk(take) else {
                break;
            };
            let (first, second) = chunk.as_slices();
            out.extend_from_slice(first);
            out.extend_from_slice(second);
            chunk.commit_all();
        }
        out.len() - before
    }
}

/// Build both halves of the tap. **Non-RT**: this is where the ring is
/// allocated.
///
/// A capacity below one stereo frame is raised to [`TAP_QUEUE_CAPACITY`];
/// `rtrb` panics on a zero-length ring, and a dead debug tap is not worth
/// crashing boot for.
pub fn debug_tap_channel(capacity: usize) -> (TapRtSide, TapConsumer) {
    let capacity = if capacity < CHANNELS {
        TAP_QUEUE_CAPACITY
    } else {
        capacity
    };
    let (producer, consumer) = RingBuffer::<f32>::new(capacity);
    let shared = Arc::new(TapShared::default());
    (
        TapRtSide {
            producer,
            shared: Arc::clone(&shared),
        },
        TapConsumer { consumer, shared },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(frames: usize, base: f32) -> (Vec<f32>, Vec<f32>) {
        let left: Vec<f32> = (0..frames).map(|i| base + i as f32).collect();
        let right: Vec<f32> = left.iter().map(|s| -s).collect();
        (left, right)
    }

    #[test]
    fn a_disarmed_tap_copies_nothing_and_drops_nothing() {
        let (mut rt, consumer) = debug_tap_channel(64);
        let (left, right) = block(8, 1.0);
        for _ in 0..100 {
            rt.push_block(&left, &right);
        }
        assert_eq!(consumer.available(), 0, "a disarmed tap must stay silent");
        assert_eq!(rt.dropped(), 0, "not copying is not dropping");
        assert!(!rt.is_armed());
    }

    #[test]
    fn an_armed_tap_round_trips_interleaved_frames() {
        let (mut rt, mut consumer) = debug_tap_channel(64);
        consumer.shared().arm();

        let (left, right) = block(4, 10.0);
        rt.push_block(&left, &right);

        let mut out = Vec::new();
        assert_eq!(consumer.drain_into(&mut out, usize::MAX), 8);
        assert_eq!(
            out,
            vec![10.0, -10.0, 11.0, -11.0, 12.0, -12.0, 13.0, -13.0]
        );
        assert_eq!(rt.dropped(), 0);
    }

    #[test]
    fn a_full_ring_drops_whole_frames_and_counts_the_samples() {
        // Capacity 8 samples = 4 frames.
        let (mut rt, mut consumer) = debug_tap_channel(8);
        consumer.shared().arm();

        let (left, right) = block(6, 0.0);
        rt.push_block(&left, &right);

        let mut out = Vec::new();
        let taken = consumer.drain_into(&mut out, usize::MAX);
        assert_eq!(taken % CHANNELS, 0, "only whole frames may be stored");
        assert!(taken > 0 && taken < 12);
        assert_eq!(rt.dropped() as usize, 12 - taken);

        // Channel alignment survived: left values are non-negative, right
        // negated.
        for frame in out.as_chunks::<2>().0 {
            assert_eq!(frame[1], -frame[0]);
        }
    }

    #[test]
    fn dropping_continues_to_be_counted_while_the_ring_stays_full() {
        let (mut rt, consumer) = debug_tap_channel(8);
        consumer.shared().arm();
        let (left, right) = block(64, 0.0);
        rt.push_block(&left, &right);
        let after_first = rt.dropped();
        rt.push_block(&left, &right);
        assert_eq!(
            rt.dropped(),
            after_first + 128,
            "a completely full ring drops the whole block"
        );
    }

    #[test]
    fn mismatched_slices_truncate_to_the_shorter_one() {
        let (mut rt, mut consumer) = debug_tap_channel(64);
        consumer.shared().arm();
        rt.push_block(&[1.0, 2.0, 3.0], &[-1.0]);
        let mut out = Vec::new();
        consumer.drain_into(&mut out, usize::MAX);
        assert_eq!(out, vec![1.0, -1.0]);
    }

    #[test]
    fn an_empty_block_is_a_no_op() {
        let (mut rt, consumer) = debug_tap_channel(64);
        consumer.shared().arm();
        rt.push_block(&[], &[]);
        assert_eq!(consumer.available(), 0);
        assert_eq!(rt.dropped(), 0);
    }

    #[test]
    fn draining_respects_the_limit_and_leaves_the_rest() {
        let (mut rt, mut consumer) = debug_tap_channel(64);
        consumer.shared().arm();
        let (left, right) = block(8, 0.0);
        rt.push_block(&left, &right);

        let mut out = Vec::new();
        assert_eq!(consumer.drain_into(&mut out, 4), 4);
        assert_eq!(consumer.available(), 12);
        consumer.drain_all();
        assert_eq!(consumer.available(), 0);
    }

    #[test]
    fn the_sample_rate_is_unknown_until_a_stream_reports_one() {
        let (rt, consumer) = debug_tap_channel(64);
        assert_eq!(consumer.shared().sample_rate(), 0.0);
        rt.set_sample_rate(44_100.0);
        assert_eq!(consumer.shared().sample_rate(), 44_100.0);
    }

    #[test]
    fn a_nonsense_sample_rate_is_rejected_rather_than_stored() {
        let (rt, consumer) = debug_tap_channel(64);
        rt.set_sample_rate(48_000.0);
        for bad in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            rt.set_sample_rate(bad);
            assert_eq!(
                consumer.shared().sample_rate(),
                0.0,
                "{bad} must read back as unknown"
            );
        }
    }

    #[test]
    fn arming_and_disarming_is_visible_from_both_sides() {
        let (rt, consumer) = debug_tap_channel(64);
        assert!(!rt.is_armed());
        consumer.shared().arm();
        assert!(rt.is_armed());
        consumer.shared().disarm();
        assert!(!rt.is_armed());
    }

    #[test]
    fn a_degenerate_capacity_falls_back_to_the_default() {
        let (rt, _consumer) = debug_tap_channel(0);
        assert_eq!(rt.free_slots(), TAP_QUEUE_CAPACITY);
    }

    #[test]
    fn the_drop_counter_can_be_reset_between_captures() {
        let (mut rt, consumer) = debug_tap_channel(8);
        consumer.shared().arm();
        let (left, right) = block(64, 0.0);
        rt.push_block(&left, &right);
        assert!(rt.dropped() > 0);
        consumer.shared().reset_dropped();
        assert_eq!(rt.dropped(), 0);
    }
}
