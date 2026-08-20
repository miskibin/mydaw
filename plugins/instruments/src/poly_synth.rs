//! `poly-synth` -- eight voices of band-limited saw with a sine sub-oscillator.
//!
//! ```text
//!   note on ──> voice ──> polyBLEP saw ──┐
//!                    └──> sine, -1 oct ──┴─> one-pole LP 3 kHz ──> ADSR ──> mix
//! ```
//!
//! # The oscillator
//! A sawtooth is the useful default waveform: it contains every harmonic, so it
//! is the one a filter has something to work with. Generated naively -- `2t - 1`
//! sampled -- it also contains every harmonic *above* Nyquist, which folds back
//! down as inharmonic aliasing that gets worse the higher you play and that no
//! later filter can remove, because the aliases are below the cutoff. PolyBLEP
//! ([`poly_blep`](crate::dsp::poly_blep)) replaces the samples either side of
//! the wrap with a band-limited step, which costs one branch and two multiplies
//! and buys 20-30 dB. `tests/spectral.rs` measures the difference rather than
//! claiming it.
//!
//! The sub-oscillator is a sine an octave down at -6 dB. It is a sine and not a
//! second saw on purpose: one partial, at a frequency the 3 kHz lowpass passes
//! untouched, so it adds weight without adding anything for the filter to
//! smear. Every classic poly synth has one for the same reason.
//!
//! # The envelope
//! Linear ADSR, fixed at 5 ms / 80 ms / 0.6 / 120 ms. Each segment crosses
//! exactly the range it is responsible for in exactly its advertised time
//! (attack `0 -> 1`, decay `1 -> 0.6`, release `0.6 -> 0`), which is what makes
//! those constants mean what a reader assumes.
//!
//! Fixed, because there is nowhere to put a knob yet: parameters need the
//! automation and parameter-queue work of the mixer phase. This is documented as
//! a limitation in the step-3 spec rather than hidden behind a "TODO".
//!
//! # Voice allocation
//! Eight voices; the same key steals its own voice (so a repeated note does not
//! consume two slots and does not double in level), otherwise the first free
//! slot, otherwise the oldest. Stealing the oldest rather than dropping the note
//! is the choice that fails audibly in the right direction: a missing note reads
//! as a broken engine, a stolen one as a busy synth.
//!
//! # RT safety
//! Fixed-size arrays, no allocation, no locks, no syscalls in
//! [`PolySynthNode::process`]. Measured in `tests/rt_alloc.rs`.

use std::f32::consts::TAU;

use std::sync::Arc;

use mydaw_rt_core::{
    NoteEvent, NoteKind, ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode,
    RtNodeFactory, SmoothedParams, DEFAULT_SMOOTHING_MS,
};

use crate::dsp::{
    advance_phase, key_to_hz, linear_step, poly_blep_saw, poly_blep_square, usable_sample_rate, Svf,
};

/// RT node type id, as stored in project files. Stable across builds.
pub const NODE_TYPE_ID: &str = "poly-synth";

/// Display name for the source picker.
pub const DISPLAY_NAME: &str = "Poly Synth";

/// Voices sounding at once.
///
/// Eight is a two-handed chord with a note to spare, and small enough that the
/// whole voice array stays in a couple of cache lines.
pub const VOICES: usize = 8;

/// Attack time: silence to full, in milliseconds.
pub const ATTACK_MS: f32 = 5.0;
/// Decay time: full to [`SUSTAIN`], in milliseconds.
pub const DECAY_MS: f32 = 80.0;
/// Level held while a key is down.
pub const SUSTAIN: f32 = 0.6;
/// Release time: [`SUSTAIN`] to silence, in milliseconds.
pub const RELEASE_MS: f32 = 120.0;

/// Per-voice lowpass cutoff.
///
/// Fixed at 3 kHz: enough to take the glassy top off a saw without making it a
/// sine, and low enough that the aliasing a naive oscillator would produce
/// *below* it would be plainly audible -- which is the point of using polyBLEP
/// rather than relying on the filter.
pub const FILTER_CUTOFF_HZ: f32 = 3_000.0;

/// Sub-oscillator level relative to the saw. -6.02 dB.
pub const SUB_MIX: f32 = 0.5;

/// How far the filter envelope can open the cutoff, in octaves at 100%.
///
/// Four. Enough that a fast envelope on a closed filter is the whole point of
/// having one, and bounded so that "100%" means something a knob can reach
/// rather than "as far as the arithmetic goes".
pub const ENV_OCTAVES: f32 = 4.0;

/// Samples between filter-coefficient updates.
///
/// The cutoff moves with the envelope, and recomputing `tan` per sample per
/// voice would be the most expensive thing in the engine by an order of
/// magnitude. Sixteen samples is a 3 kHz control rate: an envelope stepping at
/// 0.3 ms is not something an ear resolves, and it is 1/16th of the cost.
const CONTROL_FRAMES: usize = 16;

/// Parameter ids, in the order [`PARAMS`] declares them.
pub mod param {
    pub const WAVE: &str = "wave";
    pub const DETUNE: &str = "detune";
    pub const SUB: &str = "sub";
    pub const CUTOFF: &str = "cutoff";
    pub const RESONANCE: &str = "resonance";
    pub const ENV_AMOUNT: &str = "envAmount";
    pub const ATTACK: &str = "attack";
    pub const DECAY: &str = "decay";
    pub const SUSTAIN: &str = "sustain";
    pub const RELEASE: &str = "release";
}

/// What the synth exposes, in the order a panel should draw it.
///
/// The same [`ParamDef`] the effects use, so the shell's generic knob panel
/// draws this instrument without knowing anything about it -- an instrument is
/// not a different kind of thing from an effect as far as a knob is concerned,
/// and giving it a second parameter system would have been two of everything.
pub static PARAMS: &[ParamDef] = &[
    ParamDef {
        id: param::WAVE,
        name: "Wave",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        step: Some(1.0),
        // A switch: there is no waveform half way between a saw and a square,
        // and ramping towards one would spend 10 ms at a shape that does not
        // exist.
        smoothing_ms: 0.0,
    },
    ParamDef {
        id: param::DETUNE,
        name: "Detune",
        unit: "ct",
        min: 0.0,
        max: 50.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        step: None,
        smoothing_ms: DEFAULT_SMOOTHING_MS,
    },
    ParamDef {
        id: param::SUB,
        name: "Sub",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: SUB_MIX,
        curve: ParamCurve::Linear,
        step: None,
        smoothing_ms: DEFAULT_SMOOTHING_MS,
    },
    ParamDef {
        id: param::CUTOFF,
        name: "Cutoff",
        unit: "Hz",
        min: 60.0,
        max: 18_000.0,
        default: FILTER_CUTOFF_HZ,
        curve: ParamCurve::Log,
        step: None,
        smoothing_ms: DEFAULT_SMOOTHING_MS,
    },
    ParamDef {
        id: param::RESONANCE,
        name: "Res",
        unit: "",
        min: 0.5,
        max: 12.0,
        default: 0.8,
        curve: ParamCurve::Log,
        step: None,
        smoothing_ms: DEFAULT_SMOOTHING_MS,
    },
    ParamDef {
        id: param::ENV_AMOUNT,
        name: "Env",
        unit: "%",
        min: -100.0,
        max: 100.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        step: None,
        smoothing_ms: DEFAULT_SMOOTHING_MS,
    },
    ParamDef {
        id: param::ATTACK,
        name: "Attack",
        unit: "ms",
        min: 1.0,
        max: 2_000.0,
        default: ATTACK_MS,
        curve: ParamCurve::Log,
        step: None,
        smoothing_ms: 0.0,
    },
    ParamDef {
        id: param::DECAY,
        name: "Decay",
        unit: "ms",
        min: 1.0,
        max: 4_000.0,
        default: DECAY_MS,
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
        default: SUSTAIN,
        curve: ParamCurve::Linear,
        step: None,
        smoothing_ms: 0.0,
    },
    ParamDef {
        id: param::RELEASE,
        name: "Release",
        unit: "ms",
        min: 1.0,
        max: 4_000.0,
        default: RELEASE_MS,
        curve: ParamCurve::Log,
        step: None,
        smoothing_ms: 0.0,
    },
];

/// Where each parameter sits in [`PARAMS`], resolved once.
#[derive(Debug, Clone, Copy)]
struct Indices {
    wave: usize,
    detune: usize,
    sub: usize,
    cutoff: usize,
    resonance: usize,
    env_amount: usize,
    attack: usize,
    decay: usize,
    sustain: usize,
    release: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        let at = |id: &str| block.index_of(id).unwrap_or(0);
        Self {
            wave: at(param::WAVE),
            detune: at(param::DETUNE),
            sub: at(param::SUB),
            cutoff: at(param::CUTOFF),
            resonance: at(param::RESONANCE),
            env_amount: at(param::ENV_AMOUNT),
            attack: at(param::ATTACK),
            decay: at(param::DECAY),
            sustain: at(param::SUSTAIN),
            release: at(param::RELEASE),
        }
    }
}

/// Everything a voice needs from the parameters for one block.
///
/// Read once per block and handed down, rather than every voice reaching into
/// the parameter block eight times per sample: the settings cannot change
/// inside a block anyway, and this way the inner loop touches one struct on the
/// stack.
#[derive(Debug, Clone, Copy)]
struct Settings {
    adsr: Adsr,
    /// `0` saw, `1` square.
    wave: usize,
    /// Frequency ratio of the second oscillator, from the detune in cents.
    detune_ratio: f32,
    sub: f32,
    cutoff_hz: f32,
    resonance: f32,
    /// Octaves the envelope adds to the cutoff at full envelope.
    env_octaves: f32,
    sample_rate: f32,
}

/// Amplitude of one voice at velocity 1.0, before the envelope.
///
/// Chosen so a four-note chord at full velocity stays well clear of full scale
/// and a single note sits in the same ballpark as the test tone (-14 dBFS).
/// Eight full-velocity voices are louder still; see the spec's limitations.
pub const VOICE_GAIN: f32 = 0.11;

/// Where a voice is in its envelope.
///
/// A stage plus one level, rather than a level chasing a target: the difference
/// between decay and release is the *rate*, and a target-chasing envelope cannot
/// express "fall to 0.6 quickly, then to 0 slowly" without a stage anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Free.
    Idle,
    Attack,
    Decay,
    /// Key held.
    Sustain,
    /// Key released, fading out.
    Release,
}

/// Envelope rates, derived from the parameters and shared by every voice.
#[derive(Debug, Clone, Copy)]
struct Adsr {
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
}

impl Adsr {
    /// The rates the four knobs ask for.
    ///
    /// Each segment crosses exactly the range it is responsible for in exactly
    /// its stated time -- attack `0 -> 1`, decay `1 -> sustain`, release
    /// `sustain -> 0` -- which is what makes the numbers on the knobs mean what
    /// a reader assumes.
    fn from_times(
        attack_ms: f32,
        decay_ms: f32,
        sustain: f32,
        release_ms: f32,
        sample_rate: f32,
    ) -> Self {
        let sustain = sustain.clamp(0.0, 1.0);
        Self {
            attack: linear_step(1.0, attack_ms, sample_rate),
            // A sustain of 1.0 leaves the decay nothing to cross; a rate of zero
            // would be an envelope that never leaves the decay stage, so the
            // range is floored at a hair rather than at nothing.
            decay: linear_step((1.0 - sustain).max(1e-4), decay_ms, sample_rate),
            sustain,
            release: linear_step(sustain.max(1e-4), release_ms, sample_rate),
        }
    }

    fn defaults(sample_rate: f32) -> Self {
        Self::from_times(ATTACK_MS, DECAY_MS, SUSTAIN, RELEASE_MS, sample_rate)
    }
}

/// One voice: two oscillators, a sub, an envelope and a filter.
#[derive(Debug, Clone, Copy)]
struct Voice {
    stage: Stage,
    key: u8,
    channel: u8,
    phase: f32,
    increment: f32,
    /// The detuned partner of `phase`. Silent-by-identity when the detune is
    /// zero: it runs at the same frequency and the pair sums to the same saw.
    phase_b: f32,
    sub_phase: f32,
    sub_increment: f32,
    envelope: f32,
    /// `velocity * VOICE_GAIN`, fixed for the life of the note.
    amplitude: f32,
    filter: Svf,
    /// Note-ons seen when this voice started, for stealing the oldest.
    age: u64,
}

impl Voice {
    fn new(sample_rate: f32) -> Self {
        Self {
            stage: Stage::Idle,
            key: 0,
            channel: 0,
            phase: 0.0,
            increment: 0.0,
            phase_b: 0.0,
            sub_phase: 0.0,
            sub_increment: 0.0,
            envelope: 0.0,
            amplitude: 0.0,
            filter: Svf::new(FILTER_CUTOFF_HZ, 0.8, sample_rate),
            age: 0,
        }
    }

    const fn is_active(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }

    /// Whether this voice is still holding its key (as opposed to releasing).
    const fn is_held(&self) -> bool {
        matches!(self.stage, Stage::Attack | Stage::Decay | Stage::Sustain)
    }

    /// Start this voice on `key`.
    ///
    /// `fresh` restarts the oscillators from zero, which is right for a slot
    /// that was silent or stolen and wrong for the same key retriggering while
    /// it still sounds -- resetting a running oscillator's phase is a step
    /// discontinuity, i.e. a click.
    fn start(
        &mut self,
        channel: u8,
        key: u8,
        velocity: f32,
        sample_rate: f32,
        age: u64,
        fresh: bool,
    ) {
        if fresh {
            self.phase = 0.0;
            // *Together*, not spread. At a detune of zero the pair has to be
            // bit-identical to the single oscillator it replaced -- two saws a
            // quarter cycle apart sum to a quieter fundamental, which would make
            // "no detune" a different sound rather than the same one. They drift
            // apart on their own the moment the detune is anything but zero,
            // which is what the effect is.
            self.phase_b = 0.0;
            self.sub_phase = 0.0;
            self.envelope = 0.0;
            self.filter.reset();
        }
        let hz = key_to_hz(key);
        self.stage = Stage::Attack;
        self.key = key;
        self.channel = channel;
        self.increment = hz / sample_rate;
        self.sub_increment = hz / (2.0 * sample_rate);
        self.amplitude = velocity * VOICE_GAIN;
        self.age = age;
    }

    /// Let go of the key. A voice already releasing is left alone.
    fn release(&mut self) {
        if self.is_held() {
            self.stage = Stage::Release;
        }
    }

    /// Add this voice's contribution to `span`.
    fn render(&mut self, span: &mut [f32], set: &Settings) {
        if !self.is_active() {
            return;
        }
        let adsr = &set.adsr;
        let increment_b = self.increment * set.detune_ratio;
        let mut until_update = 0_usize;
        for sample in span.iter_mut() {
            // Advance the envelope first, so a voice released exactly at this
            // frame starts falling here rather than one frame late.
            match self.stage {
                Stage::Attack => {
                    self.envelope += adsr.attack;
                    if self.envelope >= 1.0 {
                        self.envelope = 1.0;
                        self.stage = Stage::Decay;
                    }
                }
                Stage::Decay => {
                    self.envelope -= adsr.decay;
                    if self.envelope <= adsr.sustain {
                        self.envelope = adsr.sustain;
                        self.stage = Stage::Sustain;
                    }
                }
                Stage::Sustain => {}
                Stage::Release => {
                    self.envelope -= adsr.release;
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

            // The filter follows the envelope, so its coefficients have to move
            // with it -- at control rate, not per sample. See `CONTROL_FRAMES`.
            if until_update == 0 {
                let octaves = set.env_octaves * self.envelope;
                let cutoff = set.cutoff_hz * 2f32.powf(octaves);
                self.filter.set(cutoff, set.resonance, set.sample_rate);
                until_update = CONTROL_FRAMES;
            }
            until_update -= 1;

            let osc = |phase: f32, increment: f32| {
                if set.wave == 0 {
                    poly_blep_saw(phase, increment)
                } else {
                    poly_blep_square(phase, increment)
                }
            };
            // Two oscillators at half level each: a detune of zero has to sound
            // exactly as loud as the single oscillator it replaced.
            let pair = 0.5 * (osc(self.phase, self.increment) + osc(self.phase_b, increment_b));
            let sub = (self.sub_phase * TAU).sin() * set.sub;
            *sample += self.filter.low(pair + sub) * self.envelope * self.amplitude;

            self.phase = advance_phase(self.phase, self.increment);
            self.phase_b = advance_phase(self.phase_b, increment_b);
            self.sub_phase = advance_phase(self.sub_phase, self.sub_increment);
        }
    }
}

/// The synth: [`VOICES`] voices summed to mono and fanned out to stereo.
///
/// A **generator**: it overwrites the bus rather than summing into it, so it is
/// only ever valid at the head of a chain.
pub struct PolySynthNode {
    voices: [Voice; VOICES],
    /// The knobs, read once per block. `None` for a synth built without a
    /// parameter block -- an offline preview, a test -- which runs at the
    /// defaults for ever.
    params: Option<SmoothedParams>,
    index: Indices,
    adsr: Adsr,
    sample_rate: f32,
    /// Note-ons seen, so "oldest" is a number rather than a guess.
    counter: u64,
    /// Voices taken from a still-sounding note because all eight were busy.
    /// Diagnostics: a rising count is the honest explanation for a chord that
    /// lost its bottom note.
    stolen_voices: u64,
}

impl PolySynthNode {
    /// Build a synth for `sample_rate`. **Non-RT** -- filters and envelope rates
    /// are derived here.
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = usable_sample_rate(sample_rate);
        Self {
            voices: [Voice::new(sample_rate); VOICES],
            params: None,
            index: Indices::new(&ParamBlock::new(PARAMS)),
            adsr: Adsr::defaults(sample_rate),
            sample_rate,
            counter: 0,
            stolen_voices: 0,
        }
    }

    /// A synth reading `params`, which the shell owns and keeps across graph
    /// rebuilds -- so a rebuilt graph picks the knobs up where they were.
    ///
    /// **Non-RT**: allocates.
    pub fn with_params(params: Arc<ParamBlock>, sample_rate: f64) -> Self {
        let index = Indices::new(&params);
        let mut node = Self::new(sample_rate);
        node.params = Some(SmoothedParams::new(params, f64::from(node.sample_rate)));
        node.index = index;
        node.adsr = node.settings().adsr;
        node
    }

    /// What the parameters say right now, or the defaults without them.
    fn settings(&self) -> Settings {
        let Some(params) = self.params.as_ref() else {
            return Settings {
                adsr: Adsr::defaults(self.sample_rate),
                wave: 0,
                detune_ratio: 1.0,
                sub: SUB_MIX,
                cutoff_hz: FILTER_CUTOFF_HZ,
                resonance: 0.8,
                env_octaves: 0.0,
                sample_rate: self.sample_rate,
            };
        };
        let cents = params.value(self.index.detune);
        Settings {
            adsr: Adsr::from_times(
                params.value(self.index.attack),
                params.value(self.index.decay),
                params.value(self.index.sustain),
                params.value(self.index.release),
                self.sample_rate,
            ),
            wave: params.choice(self.index.wave),
            detune_ratio: 2f32.powf(cents / 1_200.0),
            sub: params.value(self.index.sub),
            cutoff_hz: params.value(self.index.cutoff),
            resonance: params.value(self.index.resonance),
            env_octaves: params.value(self.index.env_amount) / 100.0 * ENV_OCTAVES,
            sample_rate: self.sample_rate,
        }
    }

    /// Start a note.
    pub fn note_on(&mut self, channel: u8, key: u8, velocity: f32) {
        let velocity = if velocity.is_finite() {
            velocity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.counter = self.counter.wrapping_add(1);
        let (index, fresh) = self.allocate(channel, key);
        let (age, sample_rate) = (self.counter, self.sample_rate);
        self.voices[index].start(channel, key, velocity, sample_rate, age, fresh);
    }

    /// Release every voice holding `key` on `channel`.
    pub fn note_off(&mut self, channel: u8, key: u8) {
        for voice in &mut self.voices {
            if voice.is_held() && voice.key == key && voice.channel == channel {
                voice.release();
            }
        }
    }

    /// Pick the slot for a new note, and whether it should start from scratch.
    ///
    /// Order: the same key if it is already sounding, then a free slot, then the
    /// oldest voice.
    fn allocate(&mut self, channel: u8, key: u8) -> (usize, bool) {
        if let Some(index) = self
            .voices
            .iter()
            .position(|v| v.is_active() && v.key == key && v.channel == channel)
        {
            // Same key: keep the oscillators running so the retrigger is an
            // envelope event rather than a click.
            return (index, false);
        }
        if let Some(index) = self.voices.iter().position(|v| !v.is_active()) {
            return (index, true);
        }
        let index = self
            .voices
            .iter()
            .enumerate()
            .min_by_key(|(_, voice)| voice.age)
            .map_or(0, |(index, _)| index);
        self.stolen_voices = self.stolen_voices.saturating_add(1);
        (index, true)
    }

    fn apply(&mut self, event: &NoteEvent) {
        match event.kind {
            NoteKind::NoteOn { key, velocity } => self.note_on(event.channel, key, velocity),
            NoteKind::NoteOff { key } => self.note_off(event.channel, key),
            // `NoteKind` is `#[non_exhaustive]`; anything else is not something
            // this synth claims to understand.
            _ => {}
        }
    }

    fn render(&mut self, span: &mut [f32], set: &Settings) {
        for voice in &mut self.voices {
            voice.render(span, set);
        }
    }

    /// Voices sounding or releasing. Diagnostics and tests.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.is_active()).count()
    }

    /// Keys currently sounding, in voice order. Diagnostics and tests.
    pub fn sounding_keys(&self) -> Vec<u8> {
        self.voices
            .iter()
            .filter(|v| v.is_active())
            .map(|v| v.key)
            .collect()
    }

    /// How many times a note has taken a still-sounding voice.
    pub fn stolen_voices(&self) -> u64 {
        self.stolen_voices
    }
}

impl RtNode for PolySynthNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        let events = ctx.events;
        let frames = ctx.frames;

        // One read of the knobs per block. A parameter cannot change inside a
        // block anyway, and a voice that had to reach into shared atomics eight
        // times per sample would be paying for a race it cannot have.
        if let Some(params) = self.params.as_mut() {
            params.begin_block();
        }
        let settings = self.settings();
        self.adsr = settings.adsr;

        let [left, right] = ctx.buffers.as_mut_slices();
        left.fill(0.0);

        // Split at every event offset, so a note at frame `n` starts at frame
        // `n` and not at the top of the block.
        let mut cursor = 0_usize;
        for event in events {
            let at = (event.frame_offset as usize).min(frames);
            if at > cursor {
                self.render(&mut left[cursor..at], &settings);
                cursor = at;
            }
            self.apply(event);
        }
        self.render(&mut left[cursor..], &settings);

        // The ramps still have to advance, or `value()` would sit at the block's
        // first reading for ever and a knob drag would never arrive.
        if let Some(params) = self.params.as_mut() {
            params.skip_all(frames);
        }

        right.copy_from_slice(left);
    }
}

/// Builds [`PolySynthNode`]s for the graph builder.
#[derive(Debug, Default, Clone, Copy)]
pub struct PolySynthFactory;

impl PolySynthFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for PolySynthFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, _max_block: usize) -> Box<dyn RtNode> {
        Box::new(PolySynthNode::new(sample_rate))
    }
}

impl ParamNodeFactory for PolySynthFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        _max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(PolySynthNode::with_params(params, sample_rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{peak, render};

    const SR: f64 = 48_000.0;
    const A4: u8 = 69;

    #[test]
    fn an_unplayed_synth_is_digitally_silent() {
        let mut node = PolySynthNode::new(SR);
        let (left, right) = render(&mut node, 4_800, 128, &[]);
        assert!(left.iter().all(|s| *s == 0.0));
        assert_eq!(left, right, "the synth fans one mono render out to stereo");
        assert_eq!(node.active_voices(), 0);
    }

    #[test]
    fn a_held_note_sounds_and_settles_on_the_sustain_level() {
        let mut node = PolySynthNode::new(SR);
        // 300 ms: past the 5 ms attack and the 80 ms decay.
        let (out, _) = render(&mut node, 14_400, 256, &[NoteEvent::note_on(0, A4, 1.0)]);
        assert!(peak(&out) > 0.05, "a held note must be audible");
        assert_eq!(node.active_voices(), 1);
        assert_eq!(node.sounding_keys(), vec![A4]);

        // The last 100 ms is pure sustain, so its peak is the steady state.
        let sustained = peak(&out[9_600..]);
        let attacked = peak(&out[..480]);
        assert!(
            sustained < attacked,
            "the decay should have brought {sustained} below the attack peak {attacked}"
        );
    }

    #[test]
    fn the_envelope_segments_take_the_advertised_time() {
        let adsr = Adsr::defaults(48_000.0);
        assert!(
            (1.0 / adsr.attack - 240.0_f32).abs() < 1e-3,
            "5 ms at 48 kHz is 240 frames"
        );
        assert!(
            ((1.0 - SUSTAIN) / adsr.decay - 3_840.0).abs() < 1e-2,
            "80 ms at 48 kHz is 3840 frames"
        );
        assert!(
            (SUSTAIN / adsr.release - 5_760.0).abs() < 1e-2,
            "120 ms at 48 kHz is 5760 frames"
        );
    }

    #[test]
    fn a_release_completes_to_exact_silence() {
        let mut node = PolySynthNode::new(SR);
        render(&mut node, 9_600, 256, &[NoteEvent::note_on(0, A4, 1.0)]);
        assert_eq!(node.active_voices(), 1);

        // Release, then wait twice the release time.
        let (releasing, _) = render(&mut node, 4_800, 256, &[NoteEvent::note_off(0, A4)]);
        assert!(peak(&releasing) > 0.0, "the tail must actually be heard");
        render(&mut node, 9_600, 256, &[]);
        assert_eq!(node.active_voices(), 0, "the voice must free itself");

        let (silence, _) = render(&mut node, 4_800, 256, &[]);
        assert!(
            silence.iter().all(|s| *s == 0.0),
            "after the release the synth must be digitally silent, peaked at {}",
            peak(&silence)
        );
    }

    #[test]
    fn a_note_off_for_a_key_that_was_never_pressed_is_harmless() {
        let mut node = PolySynthNode::new(SR);
        render(&mut node, 2_400, 256, &[NoteEvent::note_on(0, A4, 1.0)]);
        render(&mut node, 2_400, 256, &[NoteEvent::note_off(0, 60)]);
        assert_eq!(node.active_voices(), 1, "the wrong key must not release A4");
        assert_eq!(node.sounding_keys(), vec![A4]);
    }

    #[test]
    fn velocity_scales_amplitude_proportionally() {
        let mut loud = PolySynthNode::new(SR);
        let (full, _) = render(&mut loud, 9_600, 256, &[NoteEvent::note_on(0, A4, 1.0)]);
        let mut quiet = PolySynthNode::new(SR);
        let (half, _) = render(&mut quiet, 9_600, 256, &[NoteEvent::note_on(0, A4, 0.5)]);
        let ratio = peak(&half) / peak(&full);
        assert!(
            (ratio - 0.5).abs() < 1e-4,
            "half velocity should be half amplitude, got {ratio}"
        );
    }

    #[test]
    fn a_velocity_the_engine_should_never_send_is_clamped_rather_than_trusted() {
        for velocity in [-1.0_f32, 4.0, f32::NAN, f32::INFINITY] {
            let mut node = PolySynthNode::new(SR);
            let (out, _) = render(
                &mut node,
                4_800,
                256,
                &[NoteEvent::note_on(0, A4, velocity)],
            );
            assert!(out.iter().all(|s| s.is_finite()), "velocity {velocity}");
            assert!(peak(&out) <= 1.0, "velocity {velocity} escaped the range");
        }
    }

    #[test]
    fn the_same_key_pressed_twice_uses_one_voice() {
        let mut node = PolySynthNode::new(SR);
        render(&mut node, 2_400, 256, &[NoteEvent::note_on(0, A4, 1.0)]);
        render(&mut node, 2_400, 256, &[NoteEvent::note_on(0, A4, 1.0)]);
        assert_eq!(node.active_voices(), 1, "a retrigger must not stack voices");
        assert_eq!(node.stolen_voices(), 0, "its own voice is not a steal");
    }

    #[test]
    fn a_ninth_note_steals_the_oldest_voice() {
        let mut node = PolySynthNode::new(SR);
        // Eight distinct keys, one block apart so their ages differ.
        for key in 60..68_u8 {
            render(&mut node, 256, 256, &[NoteEvent::note_on(0, key, 1.0)]);
        }
        assert_eq!(node.active_voices(), VOICES);
        let mut sounding = node.sounding_keys();
        sounding.sort_unstable();
        assert_eq!(sounding, (60..68).collect::<Vec<u8>>());

        render(&mut node, 256, 256, &[NoteEvent::note_on(0, 72, 1.0)]);
        assert_eq!(node.active_voices(), VOICES, "polyphony is still eight");
        assert_eq!(node.stolen_voices(), 1);
        let mut sounding = node.sounding_keys();
        sounding.sort_unstable();
        assert_eq!(
            sounding,
            vec![61, 62, 63, 64, 65, 66, 67, 72],
            "key 60 was the oldest and is the one that had to go"
        );
    }

    #[test]
    fn a_four_note_chord_at_full_velocity_stays_below_full_scale() {
        let mut node = PolySynthNode::new(SR);
        let events: Vec<NoteEvent> = [60_u8, 64, 67, 72]
            .iter()
            .map(|k| NoteEvent::note_on(0, *k, 1.0))
            .collect();
        let (out, _) = render(&mut node, 24_000, 256, &events);
        let peak = peak(&out);
        assert!(peak < 1.0, "a four-note chord clipped at {peak}");
        assert!(
            peak > 0.2,
            "a four-note chord should be clearly audible, got {peak}"
        );
    }

    #[test]
    fn a_note_starts_at_its_frame_offset_and_not_at_the_start_of_the_block() {
        let mut node = PolySynthNode::new(SR);
        let (out, _) = render(&mut node, 512, 512, &[NoteEvent::note_on(256, A4, 1.0)]);
        assert!(
            out[..256].iter().all(|s| *s == 0.0),
            "nothing may sound before the event's frame"
        );
        assert!(
            peak(&out[256..]) > 0.0,
            "the note is missing after its frame"
        );
    }

    #[test]
    fn notes_on_different_channels_are_different_voices() {
        let mut node = PolySynthNode::new(SR);
        render(
            &mut node,
            256,
            256,
            &[
                NoteEvent::note_on(0, A4, 1.0),
                NoteEvent::note_on(0, A4, 1.0).on_channel(1),
            ],
        );
        assert_eq!(
            node.active_voices(),
            2,
            "channel is part of a note's identity"
        );

        // And a note-off on one channel leaves the other alone.
        render(&mut node, 256, 256, &[NoteEvent::note_off(0, A4)]);
        render(&mut node, 19_200, 256, &[]);
        assert_eq!(node.active_voices(), 1);
    }

    #[test]
    fn the_factory_is_registered_under_its_type_id() {
        let factory = PolySynthFactory::new();
        assert_eq!(factory.type_id(), "poly-synth");
        let mut node = factory.create(SR, 512);
        let (out, _) = render(node.as_mut(), 9_600, 512, &[NoteEvent::note_on(0, A4, 1.0)]);
        assert!(peak(&out) > 0.05);
    }
}
