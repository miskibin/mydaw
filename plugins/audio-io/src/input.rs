//! Audio *input*: the capture device's callback, the ring behind it, and the
//! conversion that turns whatever the device hands us into the engine's stereo
//! at the engine's rate.
//!
//! ```text
//!   capture thread (RT)                 recorder worker (non-RT)
//!   -------------------                 ------------------------
//!   InputRtSide::push_interleaved
//!     |-- fold_frame -> L R  ==rtrb==>  InputConsumer::drain_into
//!     |-- InputShared::observe            |
//!     '-- (dropped counter)               '-> RateConverter::push -> take buffer
//! ```
//!
//! The split is the same one [`crate::rt`] makes for output: everything here
//! that touches the callback is arithmetic over pre-allocated memory and relaxed
//! atomics, and everything that allocates lives on the worker. That is also what
//! makes this module testable on a machine with no sound card -- [`InputRtSide`]
//! takes a slice, not a device.
//!
//! # Channel policy
//! Mono is duplicated to both sides, stereo is taken as-is, and anything wider
//! keeps its first two channels. Identical to
//! [`mydaw_audio_file`](mydaw_audio_file)'s policy for decoded files, on purpose:
//! a take and a dropped WAV must not disagree about what "the input" was.
//!
//! # Rate policy
//! A take is converted to the **engine's** rate before it is written, so a
//! recording made on a 44.1 kHz interface while the output runs at 48 kHz lines
//! up with the arrangement frame for frame rather than drifting. The conversion
//! is the same Catmull-Rom kernel the sampler plays files with
//! ([`mydaw_audio_file::cubic`]) and is bit-exact when the two rates match.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use mydaw_rt_core::CHANNELS;
use rtrb::{Consumer, Producer, RingBuffer};

/// Ring depth, in samples (not frames).
///
/// 131 072 samples = 65 536 stereo frames, about 1.4 s at 48 kHz. The worker
/// drains every [`DRAIN_INTERVAL`](crate::record::DRAIN_INTERVAL), so steady-state
/// occupancy is a block or two; the rest is headroom for a scheduling hiccup on
/// a machine that is also rendering a UI.
pub const INPUT_QUEUE_CAPACITY: usize = 1 << 17;

/// How fast the peak reading falls when the input goes quiet, in dB per second.
///
/// 20 dB/s is the broadcast-console convention and is slow enough that a poll at
/// 60 ms cannot miss a transient, without leaving the bar stuck up after a take.
pub const PEAK_DECAY_DB_PER_SECOND: f32 = 20.0;

/// Block duration assumed for the decay when the device has not said what its
/// buffer is. 10 ms is a typical shared-mode period.
const ASSUMED_BLOCK_SECONDS: f32 = 0.010;

/// Fold one device frame down to the engine's stereo pair.
///
/// See the module docs for why this is the policy. A zero-channel frame -- which
/// a broken driver can report -- is silence rather than a panic.
#[inline]
pub fn fold_frame(frame: &[f32]) -> (f32, f32) {
    match frame.len() {
        0 => (0.0, 0.0),
        1 => (frame[0], frame[0]),
        _ => (frame[0], frame[1]),
    }
}

/// Per-block decay factor that loses [`PEAK_DECAY_DB_PER_SECOND`] a second.
///
/// Computed once per stream on the non-RT side: `powf` in a callback is legal
/// but pointless when the block size does not change.
pub fn peak_decay_for(block_frames: u32, sample_rate: f64) -> f32 {
    let seconds = if block_frames > 0 && sample_rate > 0.0 {
        (f64::from(block_frames) / sample_rate) as f32
    } else {
        ASSUMED_BLOCK_SECONDS
    };
    10.0_f32.powf(-PEAK_DECAY_DB_PER_SECOND * seconds / 20.0)
}

/// What went wrong with an open input stream, as a code the callback can store
/// with one relaxed write.
///
/// cpal's error callback may run on the capture thread, so it may not allocate a
/// message; the non-RT side turns the code back into the sentence a musician
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InputFault {
    /// Nothing has gone wrong.
    None = 0,
    /// The device was unplugged, or the host revoked it, mid-stream.
    DeviceLost = 1,
    /// The OS refused access -- a microphone privacy setting, or a user who is
    /// not in the audio group.
    PermissionDenied = 2,
    /// Anything else cpal reported. Rare, and not actionable beyond "try again".
    Backend = 3,
}

impl InputFault {
    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::DeviceLost,
            2 => Self::PermissionDenied,
            3 => Self::Backend,
            _ => Self::None,
        }
    }

    /// The sentence the UI shows, or empty when nothing is wrong.
    pub fn message(self) -> &'static str {
        match self {
            Self::None => "",
            Self::DeviceLost => {
                "The input device disappeared. Recording stopped; whatever had already arrived was kept."
            }
            Self::PermissionDenied => {
                "The system refused access to the input device. Check the microphone privacy setting for WAVE."
            }
            Self::Backend => "The input device reported an error and the stream was closed.",
        }
    }

    /// Whether this fault means the stream is gone rather than merely unhappy.
    pub fn is_fatal(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// State shared between the capture callback and the recorder worker.
///
/// Every field is an atomic touched with `Relaxed`: the recording flag is
/// advisory (arming a block late is inaudible), the level is a readout, and the
/// samples themselves are ordered by the `rtrb` ring's own acquire/release.
#[derive(Debug)]
pub struct InputShared {
    recording: AtomicBool,
    dropped: AtomicU64,
    frames_seen: AtomicU64,
    peak_bits: AtomicU32,
    rms_bits: AtomicU32,
    fault: AtomicU8,
}

impl Default for InputShared {
    fn default() -> Self {
        Self {
            recording: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            frames_seen: AtomicU64::new(0),
            peak_bits: AtomicU32::new(0),
            rms_bits: AtomicU32::new(0),
            fault: AtomicU8::new(InputFault::None as u8),
        }
    }
}

impl InputShared {
    /// Whether the callback is currently copying into the ring.
    #[inline]
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Relaxed)
    }

    /// Start copying. Non-RT side only.
    pub fn start(&self) {
        self.recording.store(true, Ordering::Relaxed);
    }

    /// Stop copying. Non-RT side only.
    pub fn stop(&self) {
        self.recording.store(false, Ordering::Relaxed);
    }

    /// Samples the callback threw away because the ring was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Zero the drop counter. Done at the start of every take.
    pub fn reset_dropped(&self) {
        self.dropped.store(0, Ordering::Relaxed);
    }

    /// Device frames the callback has processed since the stream opened.
    ///
    /// Nonzero is the proof that the device is actually delivering audio, which
    /// is the difference between "armed and silent" and "armed and dead".
    pub fn frames_seen(&self) -> u64 {
        self.frames_seen.load(Ordering::Relaxed)
    }

    /// Peak of the most recent blocks, with the decay applied. `0.0` when idle.
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak_bits.load(Ordering::Relaxed))
    }

    /// RMS of the most recent block. `0.0` when idle.
    pub fn rms(&self) -> f32 {
        f32::from_bits(self.rms_bits.load(Ordering::Relaxed))
    }

    /// Drop the level back to silence, so a disarmed meter does not sit lit.
    pub fn clear_level(&self) {
        self.peak_bits.store(0, Ordering::Relaxed);
        self.rms_bits.store(0, Ordering::Relaxed);
    }

    /// The last fault the stream reported.
    pub fn fault(&self) -> InputFault {
        InputFault::from_code(self.fault.load(Ordering::Relaxed))
    }

    /// Record a fault. Called from cpal's error callback, which may be the
    /// capture thread: one relaxed store, no allocation.
    #[inline]
    pub fn set_fault(&self, fault: InputFault) {
        self.fault.store(fault as u8, Ordering::Relaxed);
    }

    /// Clear the fault, done when a fresh stream opens.
    pub fn clear_fault(&self) {
        self.set_fault(InputFault::None);
    }
}

/// The capture thread's end of the input path.
///
/// Moved into the cpal input closure, exactly as [`crate::rt::RtState`] is moved
/// into the output one. Nothing here allocates, locks or blocks.
pub struct InputRtSide {
    producer: Producer<f32>,
    shared: Arc<InputShared>,
    /// Channels the device delivers. Fixed for the stream's lifetime.
    channels: usize,
    /// Per-block peak decay; see [`peak_decay_for`].
    decay: f32,
}

impl InputRtSide {
    /// Shared flags and counters, for the non-RT side.
    pub fn shared(&self) -> &Arc<InputShared> {
        &self.shared
    }

    /// Free slots in the ring, in samples. Test and diagnostic helper.
    pub fn free_slots(&self) -> usize {
        self.producer.slots()
    }

    /// Take one interleaved device block: measure it, and copy it into the ring
    /// if a take is running.
    ///
    /// # RT safety
    /// One pass over the block for the level, one chunk reservation and a
    /// straight-line copy when recording. No allocation, no lock, no syscall.
    /// A full ring costs whole frames, counted in
    /// [`InputShared::dropped`] -- a gap in a take is a nuisance, a stalled
    /// capture callback is a click in the *output* stream sharing the machine.
    #[inline]
    pub fn push_interleaved(&mut self, data: &[f32]) {
        if self.channels == 0 {
            return;
        }
        let frames = data.len() / self.channels;
        if frames == 0 {
            return;
        }
        self.shared
            .frames_seen
            .fetch_add(frames as u64, Ordering::Relaxed);

        let recording = self.shared.is_recording();
        // Whole frames only: a half-written frame would swap the channels of
        // everything after it.
        let fit = if recording {
            (self.producer.slots() / CHANNELS).min(frames)
        } else {
            0
        };

        let mut peak = 0.0_f32;
        let mut square = 0.0_f32;
        let mut written = 0_usize;

        if fit > 0 {
            // `write_chunk_uninit` cannot fail for a length the ring has room
            // for, but a `Result` is a `Result`: on the impossible branch the
            // block is still measured, just not kept.
            if let Ok(chunk) = self.producer.write_chunk_uninit(fit * CHANNELS) {
                let folded = (0..fit).flat_map(|frame| {
                    let base = frame * self.channels;
                    let (left, right) = fold_frame(&data[base..base + self.channels]);
                    [left, right]
                });
                chunk.fill_from_iter(folded);
                written = fit;
            }
        }

        for frame in 0..frames {
            let base = frame * self.channels;
            let (left, right) = fold_frame(&data[base..base + self.channels]);
            // A driver that hands us a NaN must not poison the meter for the
            // rest of the session; `max` on a NaN keeps the running value.
            if left.is_finite() {
                peak = peak.max(left.abs());
                square += left * left;
            }
            if right.is_finite() {
                peak = peak.max(right.abs());
                square += right * right;
            }
        }

        // Only a take can drop: a block the callback was never asked to keep is
        // not a hole in a recording, and counting it would make the readout
        // meaningless.
        if recording && written < frames {
            self.shared
                .dropped
                .fetch_add(((frames - written) * CHANNELS) as u64, Ordering::Relaxed);
        }

        let rms = (square / (frames * CHANNELS) as f32).sqrt();
        let held = self.shared.peak() * self.decay;
        self.shared
            .peak_bits
            .store(peak.max(held).to_bits(), Ordering::Relaxed);
        self.shared.rms_bits.store(rms.to_bits(), Ordering::Relaxed);
    }
}

/// The worker's end of the input path.
pub struct InputConsumer {
    consumer: Consumer<f32>,
    shared: Arc<InputShared>,
}

impl InputConsumer {
    /// Shared flags and counters.
    pub fn shared(&self) -> &Arc<InputShared> {
        &self.shared
    }

    /// Samples waiting in the ring.
    pub fn available(&self) -> usize {
        self.consumer.slots()
    }

    /// Throw away everything waiting.
    ///
    /// Called before a take starts, so a recording never opens with audio from
    /// whatever the room was doing while the button was merely armed.
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

    /// Move whatever is available into `out`, appending at most `limit` samples.
    ///
    /// Returns how many samples were appended.
    pub fn drain_into(&mut self, out: &mut Vec<f32>, limit: usize) -> usize {
        let before = out.len();
        while out.len() - before < limit {
            let available = self.consumer.slots();
            if available == 0 {
                break;
            }
            let take = available.min(limit - (out.len() - before));
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

/// Build both halves of the input path. **Non-RT**: this is where the ring is
/// allocated.
///
/// `channels` is what the device delivers and `block_frames` what it asks for
/// (0 = unknown, see [`peak_decay_for`]). A capacity below one stereo frame is
/// raised to [`INPUT_QUEUE_CAPACITY`]; `rtrb` panics on a zero-length ring.
pub fn input_channel(
    capacity: usize,
    channels: usize,
    block_frames: u32,
    sample_rate: f64,
) -> (InputRtSide, InputConsumer) {
    let capacity = if capacity < CHANNELS {
        INPUT_QUEUE_CAPACITY
    } else {
        capacity
    };
    let (producer, consumer) = RingBuffer::<f32>::new(capacity);
    let shared = Arc::new(InputShared::default());
    (
        InputRtSide {
            producer,
            shared: Arc::clone(&shared),
            channels,
            decay: peak_decay_for(block_frames, sample_rate),
        },
        InputConsumer { consumer, shared },
    )
}

/// Streaming rate conversion for interleaved stereo, stateful across blocks.
///
/// Catmull-Rom over four taps, the same kernel the sampler plays pitched files
/// with. Exact at the sample points, so a converter whose two rates are equal
/// reproduces its input bit for bit -- and takes the copy-only fast path anyway.
///
/// Not real-time safe: it grows a `Vec`. It belongs to the recorder's worker
/// thread, never to the callback.
pub struct RateConverter {
    /// Output frames produced per input frame, as `in_rate / out_rate`.
    step: f64,
    /// Pending input, interleaved stereo, including the taps the next output
    /// frame needs behind it.
    buf: Vec<f32>,
    /// Read position in `buf`, in frames. Always `>= 1` so tap `-1` exists.
    pos: f64,
    /// Whether the two rates are equal, in which case this is a memcpy.
    passthrough: bool,
}

impl RateConverter {
    /// Convert from `from_rate` to `to_rate`. Non-finite or non-positive rates
    /// mean "leave it alone", which is what a driver reporting nonsense should
    /// cost: an unconverted take, not a panic.
    pub fn new(from_rate: f64, to_rate: f64) -> Self {
        let usable = from_rate.is_finite() && from_rate > 0.0 && to_rate.is_finite() && to_rate > 0.0;
        let passthrough = !usable || (from_rate - to_rate).abs() < f64::EPSILON;
        Self {
            step: if usable { from_rate / to_rate } else { 1.0 },
            // One silent lead frame, so the first output lands exactly on input
            // frame 0 rather than a tap short of it.
            buf: vec![0.0; CHANNELS],
            pos: 1.0,
            passthrough,
        }
    }

    /// Whether this converter is a straight copy.
    pub fn is_passthrough(&self) -> bool {
        self.passthrough
    }

    /// Convert `input` (interleaved stereo) and append the result to `out`.
    ///
    /// Any input the kernel cannot reach yet is held until the next call or
    /// until [`finish`](Self::finish).
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        self.emit(out);
        self.trim();
    }

    /// Flush the tail and append it.
    ///
    /// Two silent frames are appended first so the kernel can reach the last
    /// real input frame; without them a take would lose its final couple of
    /// samples, which is audible as a click on a loop.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if self.passthrough {
            return;
        }
        self.buf.extend(std::iter::repeat_n(0.0, 2 * CHANNELS));
        self.emit(out);
        self.buf.clear();
        self.pos = 1.0;
    }

    fn emit(&mut self, out: &mut Vec<f32>) {
        let frames = self.buf.len() / CHANNELS;
        if frames < 4 {
            return;
        }
        // The kernel reads frames `i - 1 ..= i + 2`, so `i` may run up to
        // `frames - 3`.
        let last = (frames - 3) as f64;
        while self.pos <= last {
            let index = self.pos as usize;
            let x = (self.pos - index as f64) as f32;
            for channel in 0..CHANNELS {
                let base = (index - 1) * CHANNELS + channel;
                let taps = [
                    self.buf[base],
                    self.buf[base + CHANNELS],
                    self.buf[base + 2 * CHANNELS],
                    self.buf[base + 3 * CHANNELS],
                ];
                out.push(mydaw_audio_file::cubic(taps, x));
            }
            self.pos += self.step;
        }
    }

    /// Drop the input the kernel can no longer reach, keeping one lead frame.
    fn trim(&mut self) {
        let consumed = (self.pos as usize).saturating_sub(1);
        if consumed == 0 {
            return;
        }
        self.buf.drain(..consumed * CHANNELS);
        self.pos -= consumed as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mono_frame_is_duplicated_and_a_wide_one_keeps_its_front_pair() {
        assert_eq!(fold_frame(&[]), (0.0, 0.0));
        assert_eq!(fold_frame(&[0.5]), (0.5, 0.5));
        assert_eq!(fold_frame(&[0.5, -0.5]), (0.5, -0.5));
        assert_eq!(fold_frame(&[0.5, -0.5, 0.1, 0.2, 0.3, 0.4]), (0.5, -0.5));
    }

    #[test]
    fn a_stopped_input_measures_but_keeps_nothing() {
        let (mut rt, consumer) = input_channel(64, 2, 128, 48_000.0);
        rt.push_interleaved(&[0.5, -0.25, 0.5, -0.25]);
        assert_eq!(consumer.available(), 0, "not recording means not copying");
        assert_eq!(rt.shared().dropped(), 0, "not copying is not dropping");
        assert_eq!(rt.shared().frames_seen(), 2);
        assert!((rt.shared().peak() - 0.5).abs() < 1e-6, "the meter is live while armed");
    }

    #[test]
    fn a_recording_input_round_trips_interleaved_stereo() {
        let (mut rt, mut consumer) = input_channel(64, 2, 128, 48_000.0);
        consumer.shared().start();
        rt.push_interleaved(&[1.0, -1.0, 0.5, -0.5]);

        let mut out = Vec::new();
        assert_eq!(consumer.drain_into(&mut out, usize::MAX), 4);
        assert_eq!(out, vec![1.0, -1.0, 0.5, -0.5]);
        assert_eq!(rt.shared().dropped(), 0);
    }

    #[test]
    fn a_mono_device_records_the_same_signal_on_both_sides() {
        let (mut rt, mut consumer) = input_channel(64, 1, 128, 48_000.0);
        consumer.shared().start();
        rt.push_interleaved(&[0.25, -0.75]);

        let mut out = Vec::new();
        consumer.drain_into(&mut out, usize::MAX);
        assert_eq!(out, vec![0.25, 0.25, -0.75, -0.75]);
    }

    #[test]
    fn a_six_channel_device_records_its_front_pair() {
        let (mut rt, mut consumer) = input_channel(64, 6, 128, 48_000.0);
        consumer.shared().start();
        rt.push_interleaved(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);

        let mut out = Vec::new();
        consumer.drain_into(&mut out, usize::MAX);
        assert_eq!(out, vec![0.1, 0.2]);
    }

    #[test]
    fn a_full_ring_drops_whole_frames_and_counts_the_samples() {
        // Four samples of room, eight samples offered.
        let (mut rt, consumer) = input_channel(4, 2, 128, 48_000.0);
        consumer.shared().start();
        rt.push_interleaved(&[1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0]);
        assert_eq!(consumer.available(), 4, "two frames fit");
        assert_eq!(rt.shared().dropped(), 4, "two frames did not");
    }

    #[test]
    fn a_nan_from_a_broken_driver_does_not_poison_the_meter() {
        let (mut rt, _consumer) = input_channel(64, 2, 128, 48_000.0);
        rt.push_interleaved(&[f32::NAN, 0.4]);
        assert!((rt.shared().peak() - 0.4).abs() < 1e-6);
        assert!(rt.shared().rms().is_finite());
    }

    #[test]
    fn the_peak_holds_and_then_falls_while_the_rms_follows_the_block() {
        let (mut rt, _consumer) = input_channel(64, 2, 480, 48_000.0);
        rt.push_interleaved(&[1.0, 1.0]);
        let loud = rt.shared().peak();
        assert!((loud - 1.0).abs() < 1e-6);
        assert!((rt.shared().rms() - 1.0).abs() < 1e-6);

        rt.push_interleaved(&[0.0, 0.0]);
        let held = rt.shared().peak();
        assert!(held < loud, "the peak must fall");
        assert!(held > 0.5, "but not instantly: it is a peak hold, {held}");
        assert_eq!(rt.shared().rms(), 0.0, "the RMS is this block's, not held");
    }

    #[test]
    fn the_decay_loses_the_documented_amount_per_second() {
        // One second of blocks must cost PEAK_DECAY_DB_PER_SECOND.
        let decay = peak_decay_for(4_800, 48_000.0);
        let after_one_second = decay.powi(10);
        let db = 20.0 * after_one_second.log10();
        assert!(
            (db + PEAK_DECAY_DB_PER_SECOND).abs() < 0.01,
            "one second of decay was {db} dB"
        );
    }

    #[test]
    fn an_unknown_block_size_still_produces_a_usable_decay() {
        let decay = peak_decay_for(0, 0.0);
        assert!(decay > 0.0 && decay < 1.0, "{decay}");
    }

    #[test]
    fn faults_round_trip_through_the_atomic_and_have_sentences() {
        let shared = InputShared::default();
        assert_eq!(shared.fault(), InputFault::None);
        assert!(shared.fault().message().is_empty());
        assert!(!shared.fault().is_fatal());

        for fault in [
            InputFault::DeviceLost,
            InputFault::PermissionDenied,
            InputFault::Backend,
        ] {
            shared.set_fault(fault);
            assert_eq!(shared.fault(), fault);
            assert!(!shared.fault().message().is_empty(), "{fault:?} needs a sentence");
            assert!(shared.fault().is_fatal());
        }
        shared.clear_fault();
        assert_eq!(shared.fault(), InputFault::None);
    }

    #[test]
    fn draining_all_leaves_the_ring_empty() {
        let (mut rt, mut consumer) = input_channel(64, 2, 128, 48_000.0);
        consumer.shared().start();
        rt.push_interleaved(&[1.0, 1.0, 2.0, 2.0]);
        assert_eq!(consumer.available(), 4);
        consumer.drain_all();
        assert_eq!(consumer.available(), 0);
    }

    #[test]
    fn drain_into_honours_its_limit_and_leaves_the_rest() {
        let (mut rt, mut consumer) = input_channel(64, 2, 128, 48_000.0);
        consumer.shared().start();
        rt.push_interleaved(&[1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
        let mut out = vec![9.0];
        assert_eq!(consumer.drain_into(&mut out, 2), 2);
        assert_eq!(out, vec![9.0, 1.0, 1.0], "the limit counts appended samples");
        assert_eq!(consumer.available(), 4);
    }

    /// A ramp, so a resampler that drops or duplicates a frame is obvious.
    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|frame| [frame as f32, -(frame as f32)])
            .collect()
    }

    #[test]
    fn equal_rates_are_a_bit_exact_copy() {
        let mut converter = RateConverter::new(48_000.0, 48_000.0);
        assert!(converter.is_passthrough());
        let input = ramp(64);
        let mut out = Vec::new();
        converter.push(&input, &mut out);
        converter.finish(&mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn an_unusable_rate_leaves_the_take_alone_rather_than_panicking() {
        for (from, to) in [(0.0, 48_000.0), (48_000.0, 0.0), (f64::NAN, 48_000.0)] {
            let mut converter = RateConverter::new(from, to);
            assert!(converter.is_passthrough(), "{from} -> {to}");
            let mut out = Vec::new();
            converter.push(&[1.0, 2.0], &mut out);
            assert_eq!(out, vec![1.0, 2.0]);
        }
    }

    #[test]
    fn upsampling_lengthens_the_take_by_the_rate_ratio() {
        let mut converter = RateConverter::new(44_100.0, 48_000.0);
        assert!(!converter.is_passthrough());
        let mut out = Vec::new();
        // Fed in blocks, because that is how the worker feeds it.
        let input = ramp(4_410);
        for block in input.chunks(256) {
            converter.push(block, &mut out);
        }
        converter.finish(&mut out);
        let frames = out.len() / CHANNELS;
        assert!(
            frames.abs_diff(4_800) <= 4,
            "44.1k -> 48k of 4410 frames should be ~4800, got {frames}"
        );
    }

    #[test]
    fn downsampling_shortens_the_take_by_the_rate_ratio() {
        let mut converter = RateConverter::new(96_000.0, 48_000.0);
        let mut out = Vec::new();
        for block in ramp(9_600).chunks(512) {
            converter.push(block, &mut out);
        }
        converter.finish(&mut out);
        let frames = out.len() / CHANNELS;
        assert!(
            frames.abs_diff(4_800) <= 4,
            "96k -> 48k of 9600 frames should be ~4800, got {frames}"
        );
    }

    #[test]
    fn conversion_reproduces_a_sine_rather_than_noise() {
        // A 1 kHz sine resampled 44.1 -> 48 must still be a 1 kHz sine: compare
        // against the analytic signal at the output rate.
        let mut converter = RateConverter::new(44_100.0, 48_000.0);
        let input: Vec<f32> = (0..4_410)
            .flat_map(|frame| {
                let value =
                    (std::f64::consts::TAU * 1_000.0 * frame as f64 / 44_100.0).sin() as f32;
                [value, value]
            })
            .collect();
        let mut out = Vec::new();
        for block in input.chunks(256) {
            converter.push(block, &mut out);
        }
        converter.finish(&mut out);

        // Skip the first and last few frames: the kernel's edges see the silent
        // padding, which is correct but not the analytic signal.
        let frames = out.len() / CHANNELS;
        let mut worst = 0.0_f32;
        for frame in 8..frames.saturating_sub(8) {
            let want = (std::f64::consts::TAU * 1_000.0 * frame as f64 / 48_000.0).sin() as f32;
            worst = worst.max((out[frame * CHANNELS] - want).abs());
        }
        assert!(worst < 0.01, "worst sample error was {worst}");
    }

    #[test]
    fn conversion_gives_the_same_answer_whatever_the_block_size() {
        let input = ramp(2_000);
        let render = |chunk: usize| {
            let mut converter = RateConverter::new(44_100.0, 48_000.0);
            let mut out = Vec::new();
            for block in input.chunks(chunk * CHANNELS) {
                converter.push(block, &mut out);
            }
            converter.finish(&mut out);
            out
        };
        let one = render(64);
        let other = render(377);
        assert_eq!(one.len(), other.len(), "block size must not change the length");
        for (a, b) in one.iter().zip(&other) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }
}
