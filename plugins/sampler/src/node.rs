//! `SamplerNode` -- the voices, and the arithmetic that makes a recording of
//! one note into a keyboard.
//!
//! ```text
//!   note on ──> allocate voice ──> lookup (key, velocity) ──> sample index
//!                    │                                             │
//!                    └── step = 2^(semitones/12) · sample_rate / device_rate
//!                                                                  │
//!   per frame:  cubic interpolate at `position` ──> × layer gain ──┴──> mix
//!   note off:   linear release over `release_ms` ──> exactly 0.0 ──> free
//! ```
//!
//! # Pitch
//! A sample recorded at `root_key` played at `key` must run
//! `2^((key - root_key)/12)` times as fast -- that *is* equal temperament. Two
//! corrections ride along: `tune_cents/1200` octaves of manifest detune, and the
//! ratio between the sample's own rate and the device's, which is why nothing is
//! resampled at load (see [`crate::bank`]). All three collapse into one `step`,
//! computed once at note-on, off the inner loop.
//!
//! # Interpolation
//! Catmull-Rom, four taps. Linear interpolation on a resampled piano is audible
//! as a dull, slightly grainy top end -- it is a lowpass whose corner moves with
//! the pitch ratio. Catmull-Rom costs three more multiplies per channel per
//! frame, is C¹ continuous (so it introduces no discontinuity of its own), and
//! passes exactly through the sample points at integer positions, which is what
//! makes `step == 1.0` bit-exact playback rather than an approximation of it.
//!
//! # Velocity
//! Two jobs, split. **Layer selection** is timbre: a piano struck hard is not a
//! loud version of a piano struck softly, and the manifest's velocity spans pick
//! the recording. **Layer gain** is level, and it only trims *within* the chosen
//! layer -- [`LAYER_FLOOR`] to 1.0 across the layer's own span. The recordings
//! already carry the loudness difference *between* layers; applying a global
//! velocity curve on top of them would count it twice, which is the classic way
//! a sampled piano ends up with no quiet end.
//!
//! # RT safety
//! Fixed-size voice array, an `Arc` to an immutable bank, and arithmetic. No
//! allocation, no locks, no syscalls in [`SamplerNode::process`]; measured in
//! `tests/rt_alloc.rs` with a counting allocator rather than asserted by eye.

use std::sync::Arc;

use mydaw_rt_core::{NoteEvent, NoteKind, ProcessCtx, RtNode, RtNodeFactory};

use crate::bank::SampleBank;

/// Voices sounding at once.
///
/// 32 is a two-handed piano chord with the sustain pedal down and room to
/// spare; a sampler's voice is far cheaper than a synth's (an array read and
/// four multiplies, no oscillator), so the number that matters is musical
/// rather than budgetary.
pub const VOICES: usize = 32;

/// Gain at the bottom of a velocity layer, relative to its top. -6 dB.
///
/// The trim *inside* one layer. See the module docs on why it is not a full
/// velocity curve.
pub const LAYER_FLOOR: f32 = 0.5;

/// Fade applied to a voice that is being taken away from a still-sounding note.
///
/// A retrigger and a steal both need a slot that is currently making sound.
/// Cutting it dead is a step discontinuity -- a click. 20 ms is short enough to
/// be musically immediate and long enough to be inaudible as a transition.
pub const CHOKE_MS: f32 = 20.0;

/// Amplitude of one voice at the top of its layer. -16.5 dB.
///
/// Piano samples are recorded close to full scale, and a DAW that clips on a
/// chord is broken. The number is set by the worst realistic case rather than
/// by taste: six full-velocity notes whose partials happen to line up sum to
/// roughly five times one note, and `0.15 * 5 < 1.0`. Beyond six, headroom
/// relies on the notes being decorrelated -- which recordings of six *different*
/// pitches are, and which `tests/voices.rs` measures rather than assumes.
///
/// A single note at -16.5 dB still sits comfortably above the test tone's
/// -14 dBFS once the layer gain is taken into account, so this is headroom
/// bought from level nobody was using.
pub const VOICE_GAIN: f32 = 0.15;

/// Sample rate substituted for one that cannot be used, so that a bad device
/// report degrades to "plays at the wrong speed" rather than "divides by zero".
const FALLBACK_SAMPLE_RATE: f64 = 48_000.0;

/// Gain for `velocity` inside a layer spanning `vel_lo..=vel_hi`.
///
/// [`LAYER_FLOOR`] at the bottom of the span, 1.0 at the top, linear between.
/// A degenerate span (one velocity wide) is full gain: there is nothing to
/// interpolate across, and a silent layer would be worse than a loud one.
#[inline]
pub fn layer_gain(velocity: u8, vel_lo: u8, vel_hi: u8) -> f32 {
    let span = f32::from(vel_hi.saturating_sub(vel_lo));
    if span <= 0.0 {
        return 1.0;
    }
    let position = (f32::from(velocity) - f32::from(vel_lo)) / span;
    LAYER_FLOOR + (1.0 - LAYER_FLOOR) * position.clamp(0.0, 1.0)
}

/// Four-point Catmull-Rom at `x` in `0.0..1.0` between `taps[1]` and `taps[2]`.
///
/// Exact at the sample points: `x == 0.0` returns `taps[1]` and `x == 1.0`
/// returns `taps[2]`, which is what makes unshifted playback bit-exact.
#[inline]
pub fn cubic(taps: [f32; 4], x: f32) -> f32 {
    // Shared with the sequencer's audio-clip voices, which resample for exactly
    // the same reason: see `mydaw_audio_file`.
    mydaw_audio_file::cubic(taps, x)
}

/// Frames advanced per output frame, for `key` on a sample rooted at
/// `root_key` and recorded at `sample_rate`, played out at `device_rate`.
#[inline]
pub fn playback_step(
    key: u8,
    root_key: u8,
    tune_cents: f32,
    sample_rate: f64,
    device_rate: f64,
) -> f64 {
    let semitones = f64::from(key) - f64::from(root_key) + f64::from(tune_cents) / 100.0;
    let ratio = (semitones / 12.0).exp2();
    ratio * sample_rate / device_rate
}

/// One sample playing back.
#[derive(Debug, Clone, Copy)]
struct Voice {
    active: bool,
    /// Whether the release ramp is running -- a released key, a choke, or both.
    releasing: bool,
    /// Index into the bank's sample list.
    sample: u16,
    key: u8,
    channel: u8,
    /// Read position in *content* frames. Fractional; the interpolator takes
    /// the remainder.
    position: f64,
    /// Content frames per output frame.
    step: f64,
    /// Fixed for the life of the note: layer gain times [`VOICE_GAIN`].
    amplitude: f32,
    /// Release multiplier, 1.0 while the key is held.
    release: f32,
    /// Subtracted from `release` each frame while `releasing`.
    release_step: f32,
    /// Note-ons seen when this voice started, so "oldest" is a number.
    age: u64,
}

impl Voice {
    const fn idle() -> Self {
        Self {
            active: false,
            releasing: false,
            sample: 0,
            key: 0,
            channel: 0,
            position: 0.0,
            step: 1.0,
            amplitude: 0.0,
            release: 1.0,
            release_step: 0.0,
            age: 0,
        }
    }

    /// Whether this voice is still holding its key, as opposed to fading.
    const fn is_held(&self) -> bool {
        self.active && !self.releasing
    }

    /// Start the release ramp, over `step` per frame. Idempotent in the sense
    /// that a faster ramp always wins: a choke must not be slowed down by a
    /// release that was already running.
    fn start_release(&mut self, step: f32) {
        if !self.active {
            return;
        }
        self.releasing = true;
        self.release_step = self.release_step.max(step);
    }

    /// Add this voice's contribution to one span of the block.
    ///
    /// **RT-safe.** Returns having freed itself if the sample ran out or the
    /// release reached zero.
    fn render(&mut self, left: &mut [f32], right: &mut [f32], bank: &SampleBank) {
        if !self.active {
            return;
        }
        let Some(sample) = bank.sample(self.sample) else {
            // The bank a voice was started against is the bank it is playing;
            // an index that does not resolve is impossible. Freeing the voice
            // rather than indexing anyway is the RT-safe way to be sure.
            self.active = false;
            return;
        };
        let frames = sample.frames() as f64;

        for (out_left, out_right) in left.iter_mut().zip(right.iter_mut()) {
            if self.position >= frames || self.position < 0.0 {
                self.active = false;
                return;
            }
            // `position` is finite and below `frames`, so this cast is in range.
            let index = self.position as usize;
            let fraction = (self.position - self.position.floor()) as f32;
            let gain = self.amplitude * self.release;

            *out_left += cubic(sample.taps(index, 0), fraction) * gain;
            *out_right += cubic(sample.taps(index, 1), fraction) * gain;

            if self.releasing {
                self.release -= self.release_step;
                if self.release <= 0.0 {
                    // Exactly zero, then gone: "released" has to mean digital
                    // silence, not "quiet enough".
                    self.release = 0.0;
                    self.active = false;
                    return;
                }
            }
            self.position += self.step;
        }
    }
}

/// A sampled instrument: [`VOICES`] voices reading one shared [`SampleBank`].
///
/// A **generator**: it overwrites the bus rather than summing into it, so it is
/// only ever valid at the head of a chain.
pub struct SamplerNode {
    bank: Arc<SampleBank>,
    voices: [Voice; VOICES],
    sample_rate: f64,
    /// Release ramp step per frame, derived from the bank's `release_ms`.
    release_step: f32,
    /// Choke ramp step per frame. Always at least the release step.
    choke_step: f32,
    /// Note-ons seen, so "oldest" is a number rather than a guess.
    counter: u64,
    /// Voices taken from a still-sounding note because all of them were busy.
    stolen_voices: u64,
    /// Note-ons for which the bank had no sample at all.
    unmapped_notes: u64,
}

impl SamplerNode {
    /// Build a node playing `bank` at `sample_rate`. **Non-RT** -- the envelope
    /// rates are derived here, and the bank is shared rather than copied.
    pub fn new(bank: Arc<SampleBank>, sample_rate: f64) -> Self {
        let sample_rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            FALLBACK_SAMPLE_RATE
        };
        let release_step = ramp_step(bank.release_ms(), sample_rate);
        let choke_step = ramp_step(CHOKE_MS, sample_rate).max(release_step);
        Self {
            bank,
            voices: [Voice::idle(); VOICES],
            sample_rate,
            release_step,
            choke_step,
            counter: 0,
            stolen_voices: 0,
            unmapped_notes: 0,
        }
    }

    /// The bank this node plays.
    pub fn bank(&self) -> &Arc<SampleBank> {
        &self.bank
    }

    /// Start a note. **RT-safe.**
    pub fn note_on(&mut self, channel: u8, key: u8, velocity: f32) {
        let velocity = midi_velocity(velocity);
        let Some(index) = self.bank.index_for(key, velocity) else {
            // A key this instrument does not cover. Counted rather than
            // silently ignored: a piano that stops at A0 should be able to say
            // so when somebody plays below it.
            self.unmapped_notes = self.unmapped_notes.saturating_add(1);
            return;
        };
        // Everything the voice needs is copied out of the bank here, so the
        // borrow ends before the voice array is touched below.
        let Some((root_key, tune_cents, sample_rate, vel_lo, vel_hi)) =
            self.bank.sample(index).map(|sample| {
                let (vel_lo, vel_hi) = sample.velocity_span();
                (
                    sample.root_key(),
                    sample.tune_cents(),
                    sample.sample_rate(),
                    vel_lo,
                    vel_hi,
                )
            })
        else {
            self.unmapped_notes = self.unmapped_notes.saturating_add(1);
            return;
        };

        // A repeated note is a fresh hammer strike, not a restarted voice: the
        // old string is still ringing. Choke it and take a new slot, so the
        // transition is a 20 ms fade rather than a jump to the head of the
        // sample.
        let choke_step = self.choke_step;
        for voice in &mut self.voices {
            if voice.is_held() && voice.key == key && voice.channel == channel {
                voice.start_release(choke_step);
            }
        }

        self.counter = self.counter.wrapping_add(1);
        let slot = self.allocate();
        let step = if self.bank.unpitched() {
            sample_rate / self.sample_rate
        } else {
            playback_step(key, root_key, tune_cents, sample_rate, self.sample_rate)
        };
        let amplitude = layer_gain(velocity, vel_lo, vel_hi) * VOICE_GAIN;

        self.voices[slot] = Voice {
            active: true,
            releasing: false,
            sample: index,
            key,
            channel,
            position: 0.0,
            step,
            amplitude,
            release: 1.0,
            release_step: 0.0,
            age: self.counter,
        };
    }

    /// Release every voice holding `key` on `channel`. **RT-safe.**
    pub fn note_off(&mut self, channel: u8, key: u8) {
        let step = self.release_step;
        for voice in &mut self.voices {
            if voice.is_held() && voice.key == key && voice.channel == channel {
                voice.start_release(step);
            }
        }
    }

    /// Pick a slot: a free one, else the oldest.
    ///
    /// Stealing the oldest rather than dropping the note is the failure that
    /// points the right way: a missing note reads as a broken engine, a stolen
    /// one as a busy instrument.
    fn allocate(&mut self) -> usize {
        if let Some(index) = self.voices.iter().position(|voice| !voice.active) {
            return index;
        }
        let index = self
            .voices
            .iter()
            .enumerate()
            .min_by_key(|(_, voice)| voice.age)
            .map_or(0, |(index, _)| index);
        self.stolen_voices = self.stolen_voices.saturating_add(1);
        index
    }

    fn apply(&mut self, event: &NoteEvent) {
        match event.kind {
            NoteKind::NoteOn { key, velocity } => self.note_on(event.channel, key, velocity),
            NoteKind::NoteOff { key } => self.note_off(event.channel, key),
            // `NoteKind` is `#[non_exhaustive]`; anything else is not something
            // this sampler claims to understand.
            _ => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        // Two disjoint field borrows rather than an `Arc::clone`: the clone
        // would be a pair of atomic RMWs per span per block, bought for nothing.
        let bank = &self.bank;
        for voice in &mut self.voices {
            voice.render(left, right, bank);
        }
    }

    /// Voices sounding or releasing. Diagnostics and tests.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|voice| voice.active).count()
    }

    /// Voices still holding their key. Diagnostics and tests.
    pub fn held_voices(&self) -> usize {
        self.voices.iter().filter(|voice| voice.is_held()).count()
    }

    /// Keys currently held, in voice order. Diagnostics and tests.
    pub fn sounding_keys(&self) -> Vec<u8> {
        self.voices
            .iter()
            .filter(|voice| voice.is_held())
            .map(|voice| voice.key)
            .collect()
    }

    /// How many times a note has taken a still-sounding voice.
    pub const fn stolen_voices(&self) -> u64 {
        self.stolen_voices
    }

    /// Note-ons the bank had no sample for.
    pub const fn unmapped_notes(&self) -> u64 {
        self.unmapped_notes
    }
}

impl RtNode for SamplerNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        let events = ctx.events;
        let frames = ctx.frames;
        let [left, right] = ctx.buffers.as_mut_slices();
        left.fill(0.0);
        right.fill(0.0);

        // Split at every event offset, so a note at frame `n` starts at frame
        // `n` and not at the top of the block.
        let mut cursor = 0_usize;
        for event in events {
            let at = (event.frame_offset as usize).min(frames);
            if at > cursor {
                self.render(&mut left[cursor..at], &mut right[cursor..at]);
                cursor = at;
            }
            self.apply(event);
        }
        self.render(&mut left[cursor..], &mut right[cursor..]);
    }
}

/// Per-frame step of a linear ramp that crosses 1.0 in `ms`.
///
/// A zero or nonsensical time gives a ramp that completes in one frame, which
/// is the honest reading of "no release".
fn ramp_step(ms: f32, sample_rate: f64) -> f32 {
    if !ms.is_finite() || ms <= 0.0 {
        return 1.0;
    }
    let frames = (f64::from(ms) / 1000.0 * sample_rate).max(1.0);
    (1.0 / frames) as f32
}

/// Normalised velocity to MIDI's `0..=127`.
///
/// Clamped rather than trusted: a velocity outside the range is an engine bug,
/// and a sampler that indexed a lookup table with it would be a crash.
#[inline]
fn midi_velocity(velocity: f32) -> u8 {
    // Only NaN needs the special case: `clamp` maps +/-infinity onto the
    // bounds correctly, and a NaN velocity has no defensible loudness, so it
    // becomes the softest note rather than the loudest.
    if velocity.is_nan() {
        return 0;
    }
    (velocity.clamp(0.0, 1.0) * 127.0).round() as u8
}

/// Builds [`SamplerNode`]s for one instrument.
///
/// One factory per instrument, each holding its own `Arc` to a bank that was
/// decoded once at plugin start. `create` therefore allocates a 500-byte voice
/// array and clones an `Arc` -- it does not touch the filesystem, which is what
/// makes changing a track's instrument instant rather than a two-second stall.
pub struct SamplerFactory {
    type_id: &'static str,
    bank: Arc<SampleBank>,
}

impl SamplerFactory {
    /// A factory for `bank`, answering to `type_id`.
    ///
    /// `type_id` is `&'static str` because [`RtNodeFactory`] requires it, and
    /// instrument ids are discovered at runtime -- so the sampler registry
    /// leaks them once, at load. See [`crate::registry`].
    pub const fn new(type_id: &'static str, bank: Arc<SampleBank>) -> Self {
        Self { type_id, bank }
    }

    /// The bank this factory builds nodes for.
    pub fn bank(&self) -> &Arc<SampleBank> {
        &self.bank
    }
}

impl std::fmt::Debug for SamplerFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamplerFactory")
            .field("type_id", &self.type_id)
            .field("samples", &self.bank.len())
            .finish()
    }
}

impl RtNodeFactory for SamplerFactory {
    fn type_id(&self) -> &'static str {
        self.type_id
    }

    fn create(&self, sample_rate: f64, _max_block: usize) -> Box<dyn RtNode> {
        Box::new(SamplerNode::new(Arc::clone(&self.bank), sample_rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_played_at_its_root_key_runs_at_its_own_speed() {
        let step = playback_step(60, 60, 0.0, 48_000.0, 48_000.0);
        assert!((step - 1.0).abs() < 1e-12, "{step}");
    }

    /// The claim the whole instrument rests on: C4 rooted, A4 played, is a
    /// ratio of 2^(9/12).
    #[test]
    fn nine_semitones_up_is_two_to_the_nine_twelfths() {
        let step = playback_step(69, 60, 0.0, 48_000.0, 48_000.0);
        let expected = 2.0_f64.powf(9.0 / 12.0);
        assert!((step - expected).abs() < 1e-12, "{step} vs {expected}");
        // And an octave is exactly two.
        assert!((playback_step(72, 60, 0.0, 48_000.0, 48_000.0) - 2.0).abs() < 1e-12);
        assert!((playback_step(48, 60, 0.0, 48_000.0, 48_000.0) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn detune_and_device_rate_fold_into_the_same_step() {
        // A hundred cents is one semitone.
        let cents = playback_step(60, 60, 100.0, 48_000.0, 48_000.0);
        let semitone = playback_step(61, 60, 0.0, 48_000.0, 48_000.0);
        assert!((cents - semitone).abs() < 1e-12, "{cents} vs {semitone}");

        // A 48 kHz sample on a 96 kHz device runs at half the step.
        let halved = playback_step(60, 60, 0.0, 48_000.0, 96_000.0);
        assert!((halved - 0.5).abs() < 1e-12, "{halved}");
    }

    #[test]
    fn cubic_interpolation_passes_exactly_through_the_sample_points() {
        let taps = [0.2_f32, -0.4, 0.9, 0.1];
        assert!((cubic(taps, 0.0) - taps[1]).abs() < 1e-6);
        assert!((cubic(taps, 1.0) - taps[2]).abs() < 1e-6);
    }

    /// A straight line must interpolate to a straight line: any curvature here
    /// would be distortion the interpolator invented.
    #[test]
    fn cubic_interpolation_reproduces_a_ramp_exactly() {
        for step in 0..=10 {
            let x = step as f32 / 10.0;
            let value = cubic([0.0, 1.0, 2.0, 3.0], x);
            assert!((value - (1.0 + x)).abs() < 1e-5, "at {x}: {value}");
        }
    }

    #[test]
    fn the_layer_gain_spans_the_floor_to_unity_across_its_own_range() {
        assert!((layer_gain(1, 1, 127) - LAYER_FLOOR).abs() < 1e-6);
        assert!((layer_gain(127, 1, 127) - 1.0).abs() < 1e-6);
        assert!((layer_gain(64, 1, 127) - 0.75).abs() < 0.01);
        // Out-of-span velocities clamp rather than extrapolate.
        assert!((layer_gain(0, 64, 127) - LAYER_FLOOR).abs() < 1e-6);
        assert!((layer_gain(127, 1, 63) - 1.0).abs() < 1e-6);
        // A one-velocity layer has nothing to interpolate across.
        assert!((layer_gain(64, 64, 64) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_velocity_the_engine_should_never_send_is_clamped_rather_than_trusted() {
        assert_eq!(midi_velocity(1.0), 127);
        assert_eq!(midi_velocity(0.0), 0);
        assert_eq!(midi_velocity(-3.0), 0);
        assert_eq!(midi_velocity(9.0), 127);
        assert_eq!(midi_velocity(f32::NAN), 0);
        assert_eq!(midi_velocity(f32::INFINITY), 127);
    }

    #[test]
    fn a_ramp_crosses_unity_in_the_time_it_advertises() {
        // 300 ms at 48 kHz is 14 400 frames.
        let step = ramp_step(300.0, 48_000.0);
        assert!((1.0 / step - 14_400.0).abs() < 1.0, "{step}");
        // Nonsense collapses to one frame rather than to a division by zero.
        assert_eq!(ramp_step(0.0, 48_000.0), 1.0);
        assert_eq!(ramp_step(-1.0, 48_000.0), 1.0);
        assert_eq!(ramp_step(f32::NAN, 48_000.0), 1.0);
    }
}
