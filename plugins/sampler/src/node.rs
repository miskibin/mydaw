//! `SamplerNode` -- the voices, and the arithmetic that makes a recording of
//! one note into a keyboard.
//!
//! ```text
//!   note on ──> allocate voice ──> lookup (key, velocity) ──> sample index
//!                    │                                             │
//!                    └── step = 2^(semitones/12) · sample_rate / device_rate
//!                                                                  │
//!   per frame:  cubic interpolate at `position` ──> × layer gain × ADSR ──> mix
//!   note off:   release stage ──> exactly 0.0 ──> free   (unless one-shot)
//! ```
//!
//! # Pitch
//! A sample recorded at `root_key` played at `key` must run
//! `2^((key - root_key)/12)` times as fast -- that *is* equal temperament. Two
//! corrections ride along: `tune_cents/1200` octaves of manifest detune, and the
//! ratio between the sample's own rate and the device's, which is why nothing is
//! resampled at load (see [`crate::bank`]). All three collapse into one `step`,
//! computed at note-on, off the inner loop.
//!
//! A voice therefore holds its pitch as *semitones above its recording* and
//! derives the step from it, rather than holding the step alone: a glide is a
//! straight line in semitones (an exponential in the ratio, which is what the
//! ear hears as an even slide) and only the endpoints are keys.
//!
//! # The amplifier
//! A full linear ADSR, the same shape and the same arithmetic as the poly
//! synth's (`plugins/instruments/src/poly_synth.rs`): each segment crosses exactly the
//! range it is responsible for in exactly its stated time -- attack `0 -> 1`,
//! decay `1 -> sustain`, release `sustain -> 0`. The defaults are the sampler
//! it replaced: instant attack, full sustain, and the release the instrument's
//! manifest asks for, so a kit that sounded right before sounds the same now.
//!
//! An instrument may declare itself a **one-shot**
//! ([`InstrumentManifest::one_shot`](crate::manifest::InstrumentManifest::one_shot)),
//! which is what a drum pad is: note-off is ignored and the recording plays to
//! its end. It replaces the trick the factory kits used to need -- a 30-second
//! release, long enough that the ramp had not moved by the time the sound was
//! over.
//!
//! # Mono and glide
//! With `mono` on, the instrument has one voice: a note that arrives while
//! another is *held* takes over the voice it finds rather than starting a new
//! one, keeps its recording and its envelope, and slides the pitch to the new
//! key over `glide` milliseconds. That is the 808 slide -- the sound of two
//! overlapping notes in a trap bass line -- and it is why the envelope must not
//! retrigger: a re-struck envelope is a second note, not a bend.
//!
//! A note that arrives when nothing is held is a fresh note: new voice, new
//! envelope, pitch straight at the key. Whatever was still fading is choked, so
//! "mono" means one voice rather than one voice plus its tails.
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

use mydaw_rt_core::{
    NoteEvent, NoteKind, ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode,
    RtNodeFactory, SmoothedParams,
};

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

/// Frames between recomputations of a gliding voice's playback step.
///
/// The step is `2^(semitones/12)`, and an `exp2` per voice per sample would be
/// the most expensive thing in a sampler by an order of magnitude -- for a
/// control signal an ear cannot resolve at that rate. Sixteen frames is 3 kHz
/// at 48 kHz, and a third of a millisecond of a glide that is measured in tens
/// of them.
const GLIDE_FRAMES: usize = 16;

/// Parameter ids, in the order [`PARAMS`] declares them.
///
/// **Persisted in project files**, so renaming one loses a saved setting.
pub mod param {
    pub const ATTACK: &str = "attack";
    pub const DECAY: &str = "decay";
    pub const SUSTAIN: &str = "sustain";
    pub const RELEASE: &str = "release";
    pub const GLIDE: &str = "glide";
    pub const MONO: &str = "mono";
}

/// Attack time a sampled instrument starts at: none.
///
/// A recording already has its own attack -- that is most of what makes it
/// sound like the instrument it is -- so the envelope's job at the front of a
/// note is to stay out of the way. The knob is there for the two cases where it
/// is not: softening a transient, and swelling a sustained note in.
pub const DEFAULT_ATTACK_MS: f32 = 0.0;

/// Decay time a sampled instrument starts at.
///
/// Inert at the default sustain of 1.0 -- a decay from full to full has nothing
/// to cross -- so this is where the segment *starts* when a user lowers sustain
/// rather than a shape anything is born with.
pub const DEFAULT_DECAY_MS: f32 = 1_000.0;

/// Sustain a sampled instrument starts at: all of it.
///
/// The recording carries its own decay. An envelope that took the level down
/// under it would be decaying a decay.
pub const DEFAULT_SUSTAIN: f32 = 1.0;

/// Longest attack or decay or release the knobs reach, in milliseconds.
///
/// Eight seconds: longer than the longest sample in the factory library, which
/// is the point at which "longer" stops meaning anything.
pub const MAX_ENVELOPE_MS: f32 = 8_000.0;

/// Longest glide, in milliseconds.
///
/// A second, which is a slide so slow it is an effect rather than a note. Trap
/// 808s live between 20 and 150 ms.
pub const MAX_GLIDE_MS: f32 = 1_000.0;

/// What a sampled instrument exposes, in the order a panel should draw it.
///
/// One list for every instrument, not one per kit: an amplifier and a glide are
/// properties of *the sampler*, and a marimba that declared its own would be
/// two code paths saying the same thing. Where a kit differs is only in where
/// the knobs start -- see
/// [`InstrumentManifest::default_params`](crate::manifest::InstrumentManifest::default_params).
pub static PARAMS: &[ParamDef] = &[
    ParamDef {
        id: param::ATTACK,
        name: "Attack",
        unit: "ms",
        min: 0.0,
        max: 2_000.0,
        default: DEFAULT_ATTACK_MS,
        // Linear rather than the logarithmic taper the synth's attack uses: a
        // log axis has no room for zero, and zero is where a sampler's attack
        // belongs -- the recording's own transient is the attack.
        curve: ParamCurve::Linear,
        step: None,
        smoothing_ms: 0.0,
    },
    ParamDef {
        id: param::DECAY,
        name: "Decay",
        unit: "ms",
        min: 1.0,
        max: MAX_ENVELOPE_MS,
        default: DEFAULT_DECAY_MS,
        curve: ParamCurve::Log,
        step: None,
        smoothing_ms: 0.0,
    },
    ParamDef {
        id: param::SUSTAIN,
        name: "Sustain",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: DEFAULT_SUSTAIN,
        curve: ParamCurve::Linear,
        step: None,
        smoothing_ms: 0.0,
    },
    ParamDef {
        id: param::RELEASE,
        name: "Release",
        unit: "ms",
        min: 1.0,
        max: MAX_ENVELOPE_MS,
        // Overridden per instrument by its manifest's `release_ms`, which is
        // where a piano's damper and a marimba's ring are written down. This is
        // the answer for an instrument that says nothing.
        default: crate::manifest::DEFAULT_RELEASE_MS,
        curve: ParamCurve::Log,
        step: None,
        smoothing_ms: 0.0,
    },
    ParamDef {
        id: param::GLIDE,
        name: "Glide",
        unit: "ms",
        min: 0.0,
        max: MAX_GLIDE_MS,
        default: 0.0,
        curve: ParamCurve::Linear,
        step: None,
        smoothing_ms: 0.0,
    },
    // A switch, so it is stepped and unsmoothed: half of "mono" is not a
    // setting.
    ParamDef::toggle(param::MONO, "Mono", false),
];

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

/// Where a voice is in its envelope.
///
/// A stage plus one level, rather than a level chasing a target: the difference
/// between decay and release is the *rate*, and a target-chasing envelope
/// cannot express "fall to 0.6 quickly, then to 0 slowly" without a stage
/// anyway. The same shape the poly synth's envelope has, for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Free.
    Idle,
    Attack,
    Decay,
    /// Key held, or a one-shot playing out.
    Sustain,
    /// Fading: a released key, a choke, or both.
    Release,
}

/// Envelope rates, derived from the knobs and shared by every voice.
///
/// Each segment crosses exactly the range it is responsible for in exactly its
/// stated time -- attack `0 -> 1`, decay `1 -> sustain`, release `sustain -> 0`
/// -- which is what makes the numbers on the knobs mean what a reader assumes.
#[derive(Debug, Clone, Copy)]
struct Amp {
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
}

impl Amp {
    fn from_times(
        attack_ms: f32,
        decay_ms: f32,
        sustain: f32,
        release_ms: f32,
        sample_rate: f64,
    ) -> Self {
        let sustain = if sustain.is_finite() {
            sustain.clamp(0.0, 1.0)
        } else {
            DEFAULT_SUSTAIN
        };
        Self {
            attack: linear_step(1.0, attack_ms, sample_rate),
            // A sustain of 1.0 leaves the decay nothing to cross; a rate of
            // zero would be an envelope that never left the decay stage, so the
            // range is floored at a hair rather than at nothing.
            decay: linear_step((1.0 - sustain).max(1e-4), decay_ms, sample_rate),
            sustain,
            release: linear_step(sustain.max(1e-4), release_ms, sample_rate),
        }
    }

    /// The envelope a node with no knobs runs: the recording's own shape, and
    /// the release its manifest asks for.
    fn from_bank(bank: &SampleBank, sample_rate: f64) -> Self {
        Self::from_times(
            DEFAULT_ATTACK_MS,
            DEFAULT_DECAY_MS,
            DEFAULT_SUSTAIN,
            bank.release_ms(),
            sample_rate,
        )
    }
}

/// Everything the voices need from the knobs for one block.
#[derive(Debug, Clone, Copy)]
struct Settings {
    amp: Amp,
    /// One voice, and overlapping notes glide instead of stacking.
    mono: bool,
    /// How long a glide takes, in frames. Zero jumps.
    glide_frames: f64,
}

/// One sample playing back.
#[derive(Debug, Clone, Copy)]
struct Voice {
    stage: Stage,
    /// Index into the bank's sample list.
    sample: u16,
    key: u8,
    channel: u8,
    /// Read position in *content* frames. Fractional; the interpolator takes
    /// the remainder.
    position: f64,
    /// Content frames per output frame. Derived from [`pitch`](Self::pitch);
    /// stored because the inner loop reads it every frame.
    step: f64,
    /// The step at the recording's own pitch: `sample_rate / device_rate`.
    rate_ratio: f64,
    /// Semitones above the recording, where a glide has got to.
    pitch: f64,
    /// Semitones above the recording the glide is heading for.
    pitch_target: f64,
    /// Semitones per frame, unsigned. Zero when the pitch is where it belongs.
    pitch_rate: f64,
    /// Fixed for the life of the note: layer gain times [`VOICE_GAIN`].
    amplitude: f32,
    /// The amplifier's current level, `0.0..=1.0`.
    envelope: f32,
    /// Subtracted from `envelope` each frame while releasing. Per voice rather
    /// than per node because a choke is a release the knob did not ask for.
    release_step: f32,
    /// Note-ons seen when this voice started, so "oldest" is a number.
    age: u64,
}

impl Voice {
    const fn idle() -> Self {
        Self {
            stage: Stage::Idle,
            sample: 0,
            key: 0,
            channel: 0,
            position: 0.0,
            step: 1.0,
            rate_ratio: 1.0,
            pitch: 0.0,
            pitch_target: 0.0,
            pitch_rate: 0.0,
            amplitude: 0.0,
            envelope: 0.0,
            release_step: 0.0,
            age: 0,
        }
    }

    /// Whether this voice is making sound or about to.
    const fn is_active(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }

    /// Whether this voice is still holding its key, as opposed to fading.
    const fn is_held(&self) -> bool {
        matches!(self.stage, Stage::Attack | Stage::Decay | Stage::Sustain)
    }

    /// Start the release stage, at `step` per frame. Idempotent in the sense
    /// that a faster ramp always wins: a choke must not be slowed down by a
    /// release that was already running.
    fn start_release(&mut self, step: f32) {
        if !self.is_active() {
            return;
        }
        self.stage = Stage::Release;
        self.release_step = self.release_step.max(step);
    }

    /// Point this voice at `semitones` above its recording, arriving in
    /// `glide_frames`. Zero frames -- or a voice that is already there -- is a
    /// jump.
    fn glide_to(&mut self, semitones: f64, glide_frames: f64) {
        self.pitch_target = semitones;
        let distance = (semitones - self.pitch).abs();
        if glide_frames <= 0.0 || distance <= 0.0 {
            self.pitch = semitones;
            self.pitch_rate = 0.0;
            self.retune();
            return;
        }
        self.pitch_rate = distance / glide_frames;
    }

    /// Move a running glide on by `frames` and recompute the step.
    fn advance_pitch(&mut self, frames: f64) {
        let remaining = self.pitch_target - self.pitch;
        let travel = self.pitch_rate * frames;
        if remaining.abs() <= travel {
            self.pitch = self.pitch_target;
            self.pitch_rate = 0.0;
        } else {
            self.pitch += travel * remaining.signum();
        }
        self.retune();
    }

    /// The step this voice's current pitch asks for.
    fn retune(&mut self) {
        self.step = self.rate_ratio * (self.pitch / 12.0).exp2();
    }

    /// Add this voice's contribution to one span of the block.
    ///
    /// **RT-safe.** Returns having freed itself if the sample ran out or the
    /// envelope reached zero.
    fn render(&mut self, left: &mut [f32], right: &mut [f32], bank: &SampleBank, amp: &Amp) {
        if !self.is_active() {
            return;
        }
        let Some(sample) = bank.sample(self.sample) else {
            // The bank a voice was started against is the bank it is playing;
            // an index that does not resolve is impossible. Freeing the voice
            // rather than indexing anyway is the RT-safe way to be sure.
            self.stage = Stage::Idle;
            return;
        };
        let frames = sample.frames() as f64;
        // The glide is a control signal, updated a block-within-a-block at a
        // time; see [`GLIDE_FRAMES`].
        let mut until_glide = 0_usize;

        for (out_left, out_right) in left.iter_mut().zip(right.iter_mut()) {
            if self.position >= frames || self.position < 0.0 {
                self.stage = Stage::Idle;
                return;
            }
            if self.pitch_rate > 0.0 && until_glide == 0 {
                self.advance_pitch(GLIDE_FRAMES as f64);
                until_glide = GLIDE_FRAMES;
            }
            until_glide = until_glide.saturating_sub(1);

            match self.stage {
                Stage::Attack => {
                    self.envelope += amp.attack;
                    if self.envelope >= 1.0 {
                        self.envelope = 1.0;
                        self.stage = Stage::Decay;
                    }
                }
                Stage::Decay => {
                    self.envelope -= amp.decay;
                    if self.envelope <= amp.sustain {
                        self.envelope = amp.sustain;
                        self.stage = Stage::Sustain;
                    }
                }
                // A sustain of zero is a held note at digital silence. The slot
                // is worth more to the next note than to a voice nobody can
                // hear.
                Stage::Sustain => {
                    if self.envelope <= 0.0 {
                        self.stage = Stage::Idle;
                        return;
                    }
                }
                Stage::Release => {
                    self.envelope -= self.release_step;
                    if self.envelope <= 0.0 {
                        // Exactly zero, then gone: "released" has to mean
                        // digital silence, not "quiet enough".
                        self.envelope = 0.0;
                        self.stage = Stage::Idle;
                        return;
                    }
                }
                Stage::Idle => return,
            }

            // `position` is finite and below `frames`, so this cast is in range.
            let index = self.position as usize;
            let fraction = (self.position - self.position.floor()) as f32;
            let gain = self.amplitude * self.envelope;

            *out_left += cubic(sample.taps(index, 0), fraction) * gain;
            *out_right += cubic(sample.taps(index, 1), fraction) * gain;

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
    /// The knobs, read once per block. `None` for a node built without a
    /// parameter block -- an offline preview, a test -- which runs at the
    /// instrument's own manifest settings for ever.
    params: Option<SmoothedParams>,
    index: Indices,
    /// This block's envelope rates, so a note-on inside the block starts on the
    /// same envelope the block is rendering with.
    amp: Amp,
    /// This block's voice mode and glide length.
    mono: bool,
    glide_frames: f64,
    /// Whether note-off is ignored. A property of the instrument, not a knob:
    /// see the module docs.
    one_shot: bool,
    /// Choke ramp step per frame: what a stolen or retriggered voice fades over.
    choke_step: f32,
    /// Note-ons seen, so "oldest" is a number rather than a guess.
    counter: u64,
    /// Voices taken from a still-sounding note because all of them were busy.
    stolen_voices: u64,
    /// Note-ons for which the bank had no sample at all.
    unmapped_notes: u64,
}

/// Where each parameter sits in [`PARAMS`], resolved once.
#[derive(Debug, Clone, Copy)]
struct Indices {
    attack: usize,
    decay: usize,
    sustain: usize,
    release: usize,
    glide: usize,
    mono: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        let at = |id: &str| block.index_of(id).unwrap_or(0);
        Self {
            attack: at(param::ATTACK),
            decay: at(param::DECAY),
            sustain: at(param::SUSTAIN),
            release: at(param::RELEASE),
            glide: at(param::GLIDE),
            mono: at(param::MONO),
        }
    }
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
        let amp = Amp::from_bank(&bank, sample_rate);
        let choke_step = linear_step(1.0, CHOKE_MS, sample_rate);
        let one_shot = bank.one_shot();
        Self {
            bank,
            voices: [Voice::idle(); VOICES],
            sample_rate,
            params: None,
            index: Indices::new(&ParamBlock::new(PARAMS)),
            amp,
            mono: false,
            glide_frames: 0.0,
            one_shot,
            choke_step,
            counter: 0,
            stolen_voices: 0,
            unmapped_notes: 0,
        }
    }

    /// A node reading `params`, which the shell owns and keeps across graph
    /// rebuilds -- so a rebuilt graph picks the knobs up where they were.
    ///
    /// The block is the truth about the envelope from here on: it is seeded per
    /// instrument (see
    /// [`instrument_defaults`](crate::registry::instrument_defaults)), which is
    /// where the manifest's `release_ms` and a kit's `default_params` land.
    ///
    /// **Non-RT**: allocates.
    pub fn with_params(bank: Arc<SampleBank>, params: Arc<ParamBlock>, sample_rate: f64) -> Self {
        let index = Indices::new(&params);
        let mut node = Self::new(bank, sample_rate);
        node.params = Some(SmoothedParams::new(params, node.sample_rate));
        node.index = index;
        let settings = node.settings();
        node.apply_settings(settings);
        node
    }

    /// The bank this node plays.
    pub fn bank(&self) -> &Arc<SampleBank> {
        &self.bank
    }

    /// What the knobs say right now, or the instrument's own settings without
    /// them.
    fn settings(&self) -> Settings {
        let Some(params) = self.params.as_ref() else {
            return Settings {
                amp: Amp::from_bank(&self.bank, self.sample_rate),
                mono: false,
                glide_frames: 0.0,
            };
        };
        Settings {
            amp: Amp::from_times(
                params.value(self.index.attack),
                params.value(self.index.decay),
                params.value(self.index.sustain),
                params.value(self.index.release),
                self.sample_rate,
            ),
            mono: params.choice(self.index.mono) != 0,
            glide_frames: f64::from(params.value(self.index.glide).max(0.0)) / 1000.0
                * self.sample_rate,
        }
    }

    fn apply_settings(&mut self, settings: Settings) {
        self.amp = settings.amp;
        self.mono = settings.mono;
        self.glide_frames = settings.glide_frames;
    }

    /// Start a note. **RT-safe.**
    pub fn note_on(&mut self, channel: u8, key: u8, velocity: f32) {
        let velocity = midi_velocity(velocity);
        let Some(index) = self.bank.index_for(key, velocity) else {
            // A key this instrument does not cover. Counted rather than
            // silently ignored: a piano that stops at A0 should be able to say
            // so when somebody plays below it. Checked before the mono path so
            // that a key off the end of the instrument is off the end of it
            // whether or not something is already sounding.
            self.unmapped_notes = self.unmapped_notes.saturating_add(1);
            return;
        };
        if self.mono && self.slide_to(channel, key) {
            return;
        }
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
        // sample. In mono every tail goes, not only this key's: one voice means
        // one voice.
        let (choke_step, mono) = (self.choke_step, self.mono);
        for voice in &mut self.voices {
            let same_note = voice.key == key && voice.channel == channel;
            if voice.is_active() && (mono || (voice.is_held() && same_note)) {
                voice.start_release(choke_step);
            }
        }

        self.counter = self.counter.wrapping_add(1);
        let slot = self.allocate();
        // An unpitched bank plays every key at the recorded speed -- a drum map
        // is a table of unrelated sounds -- so its voices sit at zero semitones
        // and never glide.
        let pitch = if self.bank.unpitched() {
            0.0
        } else {
            f64::from(key) - f64::from(root_key) + f64::from(tune_cents) / 100.0
        };
        let amplitude = layer_gain(velocity, vel_lo, vel_hi) * VOICE_GAIN;

        let mut voice = Voice {
            stage: Stage::Attack,
            sample: index,
            key,
            channel,
            position: 0.0,
            step: 1.0,
            rate_ratio: sample_rate / self.sample_rate,
            pitch,
            pitch_target: pitch,
            pitch_rate: 0.0,
            amplitude,
            envelope: 0.0,
            release_step: 0.0,
            age: self.counter,
        };
        voice.retune();
        self.voices[slot] = voice;
    }

    /// Bend the sounding mono voice to `key`, if there is one to bend.
    ///
    /// The legato half of mono mode: same recording, same envelope, same
    /// level -- only the pitch moves, over the glide time. Answers `false` when
    /// nothing is held, which is the caller's signal to start a note properly.
    ///
    /// The *recording* deliberately does not change with the key. A slide from
    /// C1 to G1 on an 808 is one continuous sound; re-picking the sample at the
    /// destination would splice a second recording into the middle of it.
    fn slide_to(&mut self, channel: u8, key: u8) -> bool {
        let Some(slot) = self.voices.iter().position(Voice::is_held) else {
            return false;
        };
        // Two disjoint field borrows: the bank is read, the voice is written.
        let bank = &self.bank;
        let voice = &mut self.voices[slot];
        let target = if bank.unpitched() {
            0.0
        } else {
            match bank.sample(voice.sample) {
                Some(sample) => {
                    f64::from(key) - f64::from(sample.root_key())
                        + f64::from(sample.tune_cents()) / 100.0
                }
                None => return false,
            }
        };
        voice.key = key;
        voice.channel = channel;
        voice.glide_to(target, self.glide_frames);
        true
    }

    /// Release every voice holding `key` on `channel`. **RT-safe.**
    ///
    /// A one-shot instrument has nothing to release: the recording is the note,
    /// and how long a finger stayed on a pad is not part of it.
    pub fn note_off(&mut self, channel: u8, key: u8) {
        if self.one_shot {
            return;
        }
        let step = self.amp.release;
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
        if let Some(index) = self.voices.iter().position(|voice| !voice.is_active()) {
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
        let amp = &self.amp;
        for voice in &mut self.voices {
            voice.render(left, right, bank, amp);
        }
    }

    /// Voices sounding or releasing. Diagnostics and tests.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|voice| voice.is_active()).count()
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

    /// Playback steps of the sounding voices, in voice order.
    ///
    /// Diagnostics and tests: a glide is a moving step, and this is how a test
    /// watches it arrive.
    pub fn sounding_steps(&self) -> Vec<f64> {
        self.voices
            .iter()
            .filter(|voice| voice.is_active())
            .map(|voice| voice.step)
            .collect()
    }

    /// Envelope levels of the sounding voices, in voice order. Diagnostics and
    /// tests.
    pub fn envelopes(&self) -> Vec<f32> {
        self.voices
            .iter()
            .filter(|voice| voice.is_active())
            .map(|voice| voice.envelope)
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

        // One read of the knobs per block. A parameter cannot change inside a
        // block anyway, and a voice that reached into shared atomics per sample
        // would be paying for a race it cannot have.
        if let Some(params) = self.params.as_mut() {
            params.begin_block();
        }
        let settings = self.settings();
        self.apply_settings(settings);

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

        // The ramps still have to advance, or `value()` would sit at the
        // block's first reading for ever and a knob drag would never arrive.
        if let Some(params) = self.params.as_mut() {
            params.skip_all(frames);
        }
    }
}

/// Per-frame step of a linear ramp that crosses `range` in `ms`.
///
/// A zero or nonsensical time gives a ramp that completes in one frame, which
/// is the honest reading of "no attack" and of "no release".
fn linear_step(range: f32, ms: f32, sample_rate: f64) -> f32 {
    if !ms.is_finite() || ms <= 0.0 {
        return 1.0;
    }
    let frames = (f64::from(ms) / 1000.0 * sample_rate).max(1.0);
    (f64::from(range) / frames) as f32
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

impl ParamNodeFactory for SamplerFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        _max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(SamplerNode::with_params(
            Arc::clone(&self.bank),
            params,
            sample_rate,
        ))
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
    fn a_ramp_crosses_its_range_in_the_time_it_advertises() {
        // 300 ms at 48 kHz is 14 400 frames.
        let step = linear_step(1.0, 300.0, 48_000.0);
        assert!((1.0 / step - 14_400.0).abs() < 1.0, "{step}");
        // Half the range in the same time is half the rate.
        let half = linear_step(0.5, 300.0, 48_000.0);
        assert!((half - step / 2.0).abs() < 1e-9, "{half} vs {step}");
        // Nonsense collapses to one frame rather than to a division by zero.
        assert_eq!(linear_step(1.0, 0.0, 48_000.0), 1.0);
        assert_eq!(linear_step(1.0, -1.0, 48_000.0), 1.0);
        assert_eq!(linear_step(1.0, f32::NAN, 48_000.0), 1.0);
    }

    /// The numbers on the knobs have to mean what a reader assumes: each
    /// segment crosses exactly the range it owns in exactly its stated time.
    #[test]
    fn the_envelope_segments_take_the_advertised_time() {
        let amp = Amp::from_times(5.0, 80.0, 0.6, 120.0, 48_000.0);
        assert!(
            (1.0 / amp.attack - 240.0).abs() < 1e-2,
            "5 ms at 48 kHz is 240 frames, not {}",
            1.0 / amp.attack
        );
        assert!(
            ((1.0 - 0.6) / amp.decay - 3_840.0).abs() < 1e-1,
            "80 ms is 3840 frames, not {}",
            (1.0 - 0.6) / amp.decay
        );
        assert!(
            (0.6 / amp.release - 5_760.0).abs() < 1e-1,
            "120 ms is 5760 frames, not {}",
            0.6 / amp.release
        );
    }

    /// The default envelope is the sampler that came before it: a note starts
    /// at once, holds at full level, and fades over the manifest's release.
    #[test]
    fn the_defaults_are_the_instruments_own_shape() {
        let amp = Amp::from_times(
            DEFAULT_ATTACK_MS,
            DEFAULT_DECAY_MS,
            DEFAULT_SUSTAIN,
            400.0,
            48_000.0,
        );
        assert_eq!(amp.attack, 1.0, "no attack is one frame, not a ramp");
        assert_eq!(amp.sustain, 1.0);
        // 400 ms at 48 kHz is 19 200 frames, crossing the whole range.
        assert!((1.0 / amp.release - 19_200.0).abs() < 1.0);
    }

    /// A sustain of 1.0 leaves the decay nothing to cross, and a rate of zero
    /// would be a voice that never left the decay stage.
    #[test]
    fn a_full_sustain_still_leaves_the_decay_a_rate() {
        let amp = Amp::from_times(0.0, 1_000.0, 1.0, 300.0, 48_000.0);
        assert!(amp.decay > 0.0, "{}", amp.decay);
    }

    /// Every knob the shell will draw, with the range and the default a panel
    /// lays out from. Changing one of these ids invalidates saved projects.
    #[test]
    fn the_parameter_list_is_the_six_a_sampled_instrument_needs() {
        let ids: Vec<&str> = PARAMS.iter().map(|def| def.id).collect();
        assert_eq!(ids, ["attack", "decay", "sustain", "release", "glide", "mono"]);

        let at = |id: &str| PARAMS.iter().find(|def| def.id == id).expect(id);
        assert_eq!(at(param::ATTACK).default, 0.0);
        assert_eq!(at(param::ATTACK).min, 0.0, "zero attack must be reachable");
        assert_eq!(at(param::SUSTAIN).default, 1.0);
        assert_eq!(at(param::GLIDE).default, 0.0);
        assert!(at(param::MONO).is_switch(), "mono is on or off");
        assert_eq!(at(param::MONO).default, 0.0);
        assert_eq!(
            at(param::RELEASE).default,
            crate::manifest::DEFAULT_RELEASE_MS
        );
    }
}
