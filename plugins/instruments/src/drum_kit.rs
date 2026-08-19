//! `drum-kit` -- four synthesised drum voices, no samples anywhere.
//!
//! ```text
//!   key 36  kick        sine, 150 -> 50 Hz sweep + 2 ms noise click
//!   key 38  snare       highpassed noise burst + a 180 Hz body tone
//!   key 42  closed hat  highpassed noise, short  -- chokes the open hat
//!   key 46  open hat    highpassed noise, long
//! ```
//!
//! # Why synthesised
//! A sampled kit would be better-sounding and much worse for this project: it
//! needs assets in the repository, a decoder, a loader that can fail, and a
//! licence. Four oscillator-and-noise voices need none of that, are
//! bit-deterministic, and can be asserted on in a unit test -- "the kick's
//! energy is below 100 Hz" is a claim about DSP, not about somebody's WAV file.
//! Sample playback is a Phase 2 instrument with a project-relative asset path.
//!
//! # One voice per drum
//! Each drum owns exactly one voice slot, and a retrigger restarts it. That is
//! what a drum machine does and what a drummer physically does: you cannot hit
//! the same drum twice without the first hit stopping. It also bounds the
//! polyphony at four, which is why [`DrumKitNode`] is a fixed-size struct with
//! no allocation anywhere in it.
//!
//! # The choke
//! Closing the hi-hat physically stops the open one ringing, so key 42 kills
//! key 46's voice. It is not a mute: the open hat's envelope is switched to a
//! [`CHOKE_MS`] decay, which is fast enough to read as instant and slow enough
//! not to click. Every drum machine since the CR-78 does this, and a kit that
//! did not would let an open hat ring through a closed one -- an unmistakable
//! wrongness that no amount of "it is technically two voices" excuses.
//!
//! # RT safety
//! [`DrumKitNode::process`] is arithmetic over a fixed-size struct. No
//! allocation, no locks, no syscalls; `tests/rt_alloc.rs` measures it with a
//! counting allocator rather than trusting this paragraph.

use std::f32::consts::TAU;

use mydaw_rt_core::{NoteEvent, NoteKind, ProcessCtx, RtNode, RtNodeFactory};

use crate::dsp::{
    advance_phase, decay_coefficient, linear_step, usable_sample_rate, Noise, OnePole,
    SILENCE_THRESHOLD,
};

/// RT node type id, as stored in project files and looked up on the graph
/// builder. Stable -- renaming it invalidates saved graphs.
pub const NODE_TYPE_ID: &str = "drum-kit";

/// Display name for the source picker.
pub const DISPLAY_NAME: &str = "Drum Kit";

/// General MIDI bass drum 1.
pub const KICK_KEY: u8 = 36;
/// General MIDI acoustic snare.
pub const SNARE_KEY: u8 = 38;
/// General MIDI closed hi-hat.
pub const CLOSED_HAT_KEY: u8 = 42;
/// General MIDI open hi-hat.
pub const OPEN_HAT_KEY: u8 = 46;

/// The keys this kit answers to, low to high. Anything else is ignored.
///
/// General MIDI numbering, because it is what every drum machine, every pad
/// controller and every MIDI file already speaks; inventing a private layout
/// would make the kit unplayable from anything but our own keyboard.
pub const KEYS: [u8; 4] = [KICK_KEY, SNARE_KEY, CLOSED_HAT_KEY, OPEN_HAT_KEY];

// --- kick -------------------------------------------------------------------

/// Where the kick's pitch sweep starts.
pub const KICK_START_HZ: f32 = 150.0;
/// Where it lands, and where nearly all of the kick's energy is.
pub const KICK_END_HZ: f32 = 50.0;
/// Time for the sweep to cover 95% of its range. The "thump" is this envelope.
pub const KICK_SWEEP_MS: f32 = 40.0;
/// -60 dB time of the kick's amplitude envelope.
pub const KICK_DECAY_MS: f32 = 250.0;
/// Length of the noise transient that gives the kick its attack.
pub const KICK_CLICK_MS: f32 = 2.0;
/// Amplitude of that transient relative to the body.
pub const KICK_CLICK_GAIN: f32 = 0.3;
/// Peak amplitude of the kick at velocity 1.0.
///
/// The four gain constants are set together, so that the worst case a player
/// can produce -- all four drums on the same frame at full velocity -- stays
/// clear of full scale. There is no master gain between this node and the
/// device yet, so the headroom has to live here;
/// `the_whole_kit_hit_at_once_stays_below_full_scale` is what holds them to it.
pub const KICK_GAIN: f32 = 0.45;

// --- snare ------------------------------------------------------------------

/// -60 dB time of the snare's noise burst.
pub const SNARE_NOISE_DECAY_MS: f32 = 150.0;
/// -60 dB time of the snare's body tone. Shorter than the noise, so the drum
/// turns into rattle rather than fading as a pitched thump.
pub const SNARE_TONE_DECAY_MS: f32 = 110.0;
/// The snare's body frequency.
pub const SNARE_TONE_HZ: f32 = 180.0;
/// Highpass applied to the snare's noise, so the wires sit above the body.
pub const SNARE_HIGHPASS_HZ: f32 = 1_000.0;
/// Noise-to-tone balance.
pub const SNARE_NOISE_MIX: f32 = 0.8;
pub const SNARE_TONE_MIX: f32 = 0.5;
/// Peak amplitude of the snare at velocity 1.0. See [`KICK_GAIN`].
pub const SNARE_GAIN: f32 = 0.35;

// --- hats -------------------------------------------------------------------

/// Highpass applied to both hats. Metal, not wind.
pub const HAT_HIGHPASS_HZ: f32 = 6_000.0;
/// -60 dB time of the closed hat.
pub const CLOSED_HAT_DECAY_MS: f32 = 60.0;
/// -60 dB time of the open hat.
pub const OPEN_HAT_DECAY_MS: f32 = 350.0;
/// -60 dB time the open hat's envelope switches to when it is choked.
///
/// 4 ms puts it below [`SILENCE_THRESHOLD`] inside 7 ms even from full
/// amplitude -- instant to a listener, and still a ramp rather than a step, so
/// it does not replace a ringing hat with a click.
pub const CHOKE_MS: f32 = 4.0;
/// Peak amplitude of either hat at velocity 1.0. See [`KICK_GAIN`].
pub const HAT_GAIN: f32 = 0.2;

// --- noise seeds ------------------------------------------------------------
//
// One per voice, re-applied on every trigger. Different seeds so the snare and
// the hats do not share a waveform (two voices playing the *same* noise sum
// coherently and sound like one louder voice); fixed seeds so a hit is
// reproducible to the bit.

pub(crate) const KICK_SEED: u32 = 0x4B49_434B; // "KICK"
pub(crate) const SNARE_SEED: u32 = 0x534E_4152; // "SNAR"
pub(crate) const CLOSED_HAT_SEED: u32 = 0x4348_4154; // "CHAT"
pub(crate) const OPEN_HAT_SEED: u32 = 0x4F48_4154; // "OHAT"

/// A drum this kit can play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drum {
    Kick,
    Snare,
    ClosedHat,
    OpenHat,
}

impl Drum {
    /// The drum a key addresses, or `None` for a key this kit ignores.
    pub const fn from_key(key: u8) -> Option<Self> {
        match key {
            KICK_KEY => Some(Self::Kick),
            SNARE_KEY => Some(Self::Snare),
            CLOSED_HAT_KEY => Some(Self::ClosedHat),
            OPEN_HAT_KEY => Some(Self::OpenHat),
            _ => None,
        }
    }

    /// The key that plays this drum.
    pub const fn key(self) -> u8 {
        match self {
            Self::Kick => KICK_KEY,
            Self::Snare => SNARE_KEY,
            Self::ClosedHat => CLOSED_HAT_KEY,
            Self::OpenHat => OPEN_HAT_KEY,
        }
    }

    /// Human-readable name, for hints and diagnostics.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Kick => "Kick",
            Self::Snare => "Snare",
            Self::ClosedHat => "Closed Hat",
            Self::OpenHat => "Open Hat",
        }
    }
}

/// Sine body with an exponential pitch sweep, plus a noise click.
///
/// The sweep is what makes a sine a kick drum: 150 Hz has a transient your ear
/// reads as an attack, 50 Hz has the weight, and the ~13 ms time constant
/// between them is the difference between a kick and a bass note.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KickVoice {
    active: bool,
    phase: f32,
    /// Sweep position, 1.0 at the transient and decaying to 0.0.
    sweep: f32,
    sweep_coefficient: f32,
    amplitude: f32,
    decay_coefficient: f32,
    /// Click envelope, a linear ramp from 1.0 to 0.0.
    click: f32,
    click_step: f32,
    noise: Noise,
    velocity: f32,
    /// Frequency multiplier for this hit, `1.0` inside the kit.
    ///
    /// The per-drum instruments ([`crate::single_drum`]) let a note key detune
    /// the kick by up to an octave either way; the kit itself always passes
    /// `1.0`, because inside a kit the key *chooses which drum* and cannot also
    /// mean a pitch.
    pitch_ratio: f32,
    inverse_sample_rate: f32,
}

impl KickVoice {
    pub(crate) fn new(sample_rate: f32) -> Self {
        Self {
            active: false,
            phase: 0.0,
            sweep: 0.0,
            // The sweep is "done" (95% of the way) at KICK_SWEEP_MS, which makes
            // the constant mean what its name says.
            sweep_coefficient: 0.05_f32
                .powf(1.0 / ((KICK_SWEEP_MS / 1000.0) * sample_rate).max(1.0)),
            amplitude: 0.0,
            decay_coefficient: decay_coefficient(KICK_DECAY_MS, sample_rate),
            click: 0.0,
            click_step: linear_step(1.0, KICK_CLICK_MS, sample_rate),
            noise: Noise::new(KICK_SEED),
            velocity: 0.0,
            pitch_ratio: 1.0,
            inverse_sample_rate: 1.0 / sample_rate,
        }
    }

    pub(crate) fn trigger(&mut self, velocity: f32) {
        self.trigger_pitched(velocity, 1.0);
    }

    pub(crate) fn trigger_pitched(&mut self, velocity: f32, pitch_ratio: f32) {
        self.active = true;
        // Phase zero is where a sine is zero, so a hard start is not a step and
        // needs no attack ramp -- the click is deliberate, a discontinuity is not.
        self.phase = 0.0;
        self.sweep = 1.0;
        self.amplitude = 1.0;
        self.click = 1.0;
        self.noise.reseed(KICK_SEED);
        self.velocity = velocity;
        self.pitch_ratio = pitch_ratio;
    }

    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn render(&mut self, span: &mut [f32]) {
        if !self.active {
            return;
        }
        let gain = self.velocity * KICK_GAIN;
        for sample in span.iter_mut() {
            // Per sample, not per span: a voice that ended halfway through a
            // block must end at that *frame*, or the output would depend on
            // the block size the device happened to ask for.
            if self.amplitude < SILENCE_THRESHOLD {
                self.active = false;
                return;
            }
            let hz = (KICK_END_HZ + (KICK_START_HZ - KICK_END_HZ) * self.sweep) * self.pitch_ratio;
            let body = (self.phase * TAU).sin() * self.amplitude;
            let click = self.noise.next_bipolar() * self.click * KICK_CLICK_GAIN;
            *sample += (body + click) * gain;

            self.phase = advance_phase(self.phase, hz * self.inverse_sample_rate);
            self.sweep *= self.sweep_coefficient;
            self.amplitude *= self.decay_coefficient;
            self.click = (self.click - self.click_step).max(0.0);
        }
    }
}

/// Highpassed noise plus a decaying 180 Hz body tone.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SnareVoice {
    active: bool,
    noise: Noise,
    highpass: OnePole,
    noise_amplitude: f32,
    noise_coefficient: f32,
    tone_phase: f32,
    /// Phase increment at the drum's own pitch, before the per-hit ratio.
    base_increment: f32,
    tone_increment: f32,
    tone_amplitude: f32,
    tone_coefficient: f32,
    velocity: f32,
}

impl SnareVoice {
    pub(crate) fn new(sample_rate: f32) -> Self {
        Self {
            active: false,
            noise: Noise::new(SNARE_SEED),
            highpass: OnePole::new(SNARE_HIGHPASS_HZ, sample_rate),
            noise_amplitude: 0.0,
            noise_coefficient: decay_coefficient(SNARE_NOISE_DECAY_MS, sample_rate),
            tone_phase: 0.0,
            base_increment: SNARE_TONE_HZ / sample_rate,
            tone_increment: SNARE_TONE_HZ / sample_rate,
            tone_amplitude: 0.0,
            tone_coefficient: decay_coefficient(SNARE_TONE_DECAY_MS, sample_rate),
            velocity: 0.0,
        }
    }

    pub(crate) fn trigger(&mut self, velocity: f32) {
        self.trigger_pitched(velocity, 1.0);
    }

    /// Hit the snare with its body tone scaled by `pitch_ratio`.
    ///
    /// Only the tone moves; the noise burst that is most of a snare is
    /// unpitched, and highpassing it at a key-dependent frequency would be a
    /// filter sweep dressed up as tuning.
    pub(crate) fn trigger_pitched(&mut self, velocity: f32, pitch_ratio: f32) {
        self.active = true;
        self.noise.reseed(SNARE_SEED);
        self.highpass.reset();
        self.noise_amplitude = 1.0;
        self.tone_phase = 0.0;
        self.tone_increment = self.base_increment * pitch_ratio;
        self.tone_amplitude = 1.0;
        self.velocity = velocity;
    }

    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn render(&mut self, span: &mut [f32]) {
        if !self.active {
            return;
        }
        let gain = self.velocity * SNARE_GAIN;
        for sample in span.iter_mut() {
            // The noise outlasts the tone, so it alone decides when the voice
            // ends -- checked per sample, so the ending frame does not move
            // with the block size.
            if self.noise_amplitude < SILENCE_THRESHOLD {
                self.active = false;
                return;
            }
            let wires = self.highpass.high(self.noise.next_bipolar())
                * self.noise_amplitude
                * SNARE_NOISE_MIX;
            let body = (self.tone_phase * TAU).sin() * self.tone_amplitude * SNARE_TONE_MIX;
            *sample += (wires + body) * gain;

            self.tone_phase = advance_phase(self.tone_phase, self.tone_increment);
            self.noise_amplitude *= self.noise_coefficient;
            self.tone_amplitude *= self.tone_coefficient;
        }
    }
}

/// Highpassed noise with an exponential decay. Both hats, one type.
///
/// The only difference between a closed and an open hat here is the decay time,
/// which is also the only difference a listener can name.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HatVoice {
    active: bool,
    noise: Noise,
    seed: u32,
    highpass: OnePole,
    amplitude: f32,
    /// The coefficient in use, which the choke temporarily replaces.
    coefficient: f32,
    open_coefficient: f32,
    choke_coefficient: f32,
    velocity: f32,
}

impl HatVoice {
    pub(crate) fn new(decay_ms: f32, seed: u32, sample_rate: f32) -> Self {
        Self {
            active: false,
            noise: Noise::new(seed),
            seed,
            highpass: OnePole::new(HAT_HIGHPASS_HZ, sample_rate),
            amplitude: 0.0,
            coefficient: decay_coefficient(decay_ms, sample_rate),
            open_coefficient: decay_coefficient(decay_ms, sample_rate),
            choke_coefficient: decay_coefficient(CHOKE_MS, sample_rate),
            velocity: 0.0,
        }
    }

    pub(crate) fn trigger(&mut self, velocity: f32) {
        self.active = true;
        self.noise.reseed(self.seed);
        self.highpass.reset();
        self.amplitude = 1.0;
        // A retrigger cancels a choke: hitting the open hat again reopens it.
        self.coefficient = self.open_coefficient;
        self.velocity = velocity;
    }

    /// Cut this voice short. Harmless when it is not sounding.
    pub(crate) fn choke(&mut self) {
        if self.active {
            self.coefficient = self.choke_coefficient;
        }
    }

    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn render(&mut self, span: &mut [f32], gain: f32) {
        if !self.active {
            return;
        }
        let gain = self.velocity * gain;
        for sample in span.iter_mut() {
            // Per sample; see `KickVoice::render`.
            if self.amplitude < SILENCE_THRESHOLD {
                self.active = false;
                return;
            }
            *sample += self.highpass.high(self.noise.next_bipolar()) * self.amplitude * gain;
            self.amplitude *= self.coefficient;
        }
    }
}

/// The kit: four voices summed to a mono signal and fanned out to stereo.
///
/// A **generator**, so it overwrites the bus rather than summing into it -- it
/// is only ever valid at the head of a chain, which is all the Phase 1 serial
/// graph has.
pub struct DrumKitNode {
    kick: KickVoice,
    snare: SnareVoice,
    closed_hat: HatVoice,
    open_hat: HatVoice,
    /// Note events addressed to a key this kit has no drum for. Diagnostics: a
    /// silent kit with a rising count means the wrong keys are being played,
    /// which is a very different problem from a kit that does not work.
    unmapped_notes: u64,
}

impl DrumKitNode {
    /// Build a kit for `sample_rate`. **Non-RT** -- every coefficient in every
    /// voice is derived here.
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = usable_sample_rate(sample_rate);
        Self {
            kick: KickVoice::new(sample_rate),
            snare: SnareVoice::new(sample_rate),
            closed_hat: HatVoice::new(CLOSED_HAT_DECAY_MS, CLOSED_HAT_SEED, sample_rate),
            open_hat: HatVoice::new(OPEN_HAT_DECAY_MS, OPEN_HAT_SEED, sample_rate),
            unmapped_notes: 0,
        }
    }

    /// Hit a drum. Velocity is clamped rather than trusted: this is reachable
    /// from a plugin's note output as well as from our own validated commands.
    pub fn trigger(&mut self, drum: Drum, velocity: f32) {
        let velocity = if velocity.is_finite() {
            velocity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        match drum {
            Drum::Kick => self.kick.trigger(velocity),
            Drum::Snare => self.snare.trigger(velocity),
            Drum::ClosedHat => {
                self.closed_hat.trigger(velocity);
                // The physical fact this models: you cannot have both hats open.
                self.open_hat.choke();
            }
            Drum::OpenHat => self.open_hat.trigger(velocity),
        }
    }

    /// Apply one event. Note-offs are ignored: a drum is a one-shot, and its
    /// length is a property of the drum rather than of how long a pad was held.
    fn apply(&mut self, event: &NoteEvent) {
        match event.kind {
            NoteKind::NoteOn { key, velocity } => match Drum::from_key(key) {
                Some(drum) => self.trigger(drum, velocity),
                None => self.unmapped_notes = self.unmapped_notes.saturating_add(1),
            },
            NoteKind::NoteOff { .. } => {}
            // `NoteKind` is `#[non_exhaustive]`; a choke or expression event
            // arriving here is not something this kit claims to understand.
            _ => {}
        }
    }

    /// Sum every active voice into `span`, which the caller has zeroed.
    fn render(&mut self, span: &mut [f32]) {
        self.kick.render(span);
        self.snare.render(span);
        self.closed_hat.render(span, HAT_GAIN);
        self.open_hat.render(span, HAT_GAIN);
    }

    /// Voices currently sounding. Diagnostics and tests.
    pub fn active_voices(&self) -> usize {
        usize::from(self.kick.active)
            + usize::from(self.snare.active)
            + usize::from(self.closed_hat.active)
            + usize::from(self.open_hat.active)
    }

    /// Whether a particular drum is sounding.
    pub fn is_sounding(&self, drum: Drum) -> bool {
        match drum {
            Drum::Kick => self.kick.active,
            Drum::Snare => self.snare.active,
            Drum::ClosedHat => self.closed_hat.active,
            Drum::OpenHat => self.open_hat.active,
        }
    }

    /// Note-ons this kit had no drum for. See [`Self::unmapped_notes`].
    pub fn unmapped_notes(&self) -> u64 {
        self.unmapped_notes
    }
}

impl RtNode for DrumKitNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        // Copied out before the buffers are borrowed mutably; both are plain
        // reads of `ProcessCtx` fields, so nothing is kept borrowed.
        let events = ctx.events;
        let frames = ctx.frames;
        let [left, right] = ctx.buffers.as_mut_slices();
        left.fill(0.0);

        // Split the block at every event offset and render each span
        // separately, so a hit at frame `n` is heard at frame `n`. Live input
        // is always at offset 0 today; the sequencer will not be.
        let mut cursor = 0_usize;
        for event in events {
            let at = (event.frame_offset as usize).min(frames);
            if at > cursor {
                self.render(&mut left[cursor..at]);
                cursor = at;
            }
            self.apply(event);
        }
        self.render(&mut left[cursor..]);

        // The kit has nothing stereo to say, so it renders once and fans out.
        right.copy_from_slice(left);
    }
}

/// Builds [`DrumKitNode`]s for the graph builder.
#[derive(Debug, Default, Clone, Copy)]
pub struct DrumKitFactory;

impl DrumKitFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for DrumKitFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, _max_block: usize) -> Box<dyn RtNode> {
        // `max_block` is irrelevant: the kit holds no per-block state and
        // writes only into the caller's buffers.
        Box::new(DrumKitNode::new(sample_rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{peak, render};

    const SR: f64 = 48_000.0;

    #[test]
    fn general_midi_keys_map_to_the_drums_they_name() {
        assert_eq!(Drum::from_key(36), Some(Drum::Kick));
        assert_eq!(Drum::from_key(38), Some(Drum::Snare));
        assert_eq!(Drum::from_key(42), Some(Drum::ClosedHat));
        assert_eq!(Drum::from_key(46), Some(Drum::OpenHat));
        for key in [0_u8, 35, 37, 40, 44, 60, 127] {
            assert_eq!(Drum::from_key(key), None, "key {key} is not in this kit");
        }
        for drum in [Drum::Kick, Drum::Snare, Drum::ClosedHat, Drum::OpenHat] {
            assert_eq!(Drum::from_key(drum.key()), Some(drum));
            assert!(KEYS.contains(&drum.key()));
            assert!(!drum.label().is_empty());
        }
    }

    #[test]
    fn an_untriggered_kit_is_digitally_silent() {
        let mut node = DrumKitNode::new(SR);
        let (left, right) = render(&mut node, 4_800, 128, &[]);
        assert!(left.iter().all(|s| *s == 0.0), "no note, no sound");
        assert_eq!(left, right);
        assert_eq!(node.active_voices(), 0);
    }

    #[test]
    fn every_drum_sounds_and_then_stops_at_its_advertised_decay() {
        // One second. A decay constant is the -60 dB time, and a voice is freed
        // at -100 dB (`SILENCE_THRESHOLD`), so the longest drum -- the 350 ms
        // open hat -- actually finishes at 350 * 100/60 = 583 ms.
        for drum in [Drum::Kick, Drum::Snare, Drum::ClosedHat, Drum::OpenHat] {
            let mut node = DrumKitNode::new(SR);
            let events = [NoteEvent::note_on(0, drum.key(), 1.0)];
            let (hit, _) = render(&mut node, 48_000, 256, &events);
            assert!(
                peak(&hit) > 0.05,
                "{} produced nothing (peak {})",
                drum.label(),
                peak(&hit)
            );
            assert!(
                !node.is_sounding(drum),
                "{} was still sounding after one second",
                drum.label()
            );

            // And the tail really is digital silence, not just quiet.
            let (tail, _) = render(&mut node, 4_800, 256, &[]);
            assert!(
                tail.iter().all(|s| *s == 0.0),
                "{} left a tail",
                drum.label()
            );
        }
    }

    #[test]
    fn a_hit_is_loud_enough_to_hear_and_never_reaches_full_scale() {
        for drum in [Drum::Kick, Drum::Snare, Drum::ClosedHat, Drum::OpenHat] {
            let mut node = DrumKitNode::new(SR);
            let (hit, _) = render(
                &mut node,
                24_000,
                256,
                &[NoteEvent::note_on(0, drum.key(), 1.0)],
            );
            let peak = peak(&hit);
            assert!(
                (0.1..1.0).contains(&peak),
                "{} peaked at {peak}, which is either inaudible or clipping",
                drum.label()
            );
        }
    }

    #[test]
    fn the_whole_kit_hit_at_once_stays_below_full_scale() {
        // Kick, snare and both hats on the same frame is the worst case a
        // player can produce, and it must not clip: there is no master gain
        // between this node and the device yet.
        let mut node = DrumKitNode::new(SR);
        let events: Vec<NoteEvent> = KEYS
            .iter()
            .map(|k| NoteEvent::note_on(0, *k, 1.0))
            .collect();
        let (hit, _) = render(&mut node, 24_000, 256, &events);
        let peak = peak(&hit);
        assert!(peak < 1.0, "the full kit clipped at {peak}");
        assert!(hit.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn velocity_scales_amplitude_proportionally() {
        let mut loud = DrumKitNode::new(SR);
        let (full, _) = render(
            &mut loud,
            12_000,
            256,
            &[NoteEvent::note_on(0, KICK_KEY, 1.0)],
        );
        let mut quiet = DrumKitNode::new(SR);
        let (half, _) = render(
            &mut quiet,
            12_000,
            256,
            &[NoteEvent::note_on(0, KICK_KEY, 0.5)],
        );

        let ratio = peak(&half) / peak(&full);
        assert!(
            (ratio - 0.5).abs() < 1e-4,
            "half velocity should be half amplitude, got {ratio}"
        );
    }

    #[test]
    fn a_velocity_the_engine_should_never_send_is_clamped_rather_than_trusted() {
        for velocity in [-1.0_f32, 4.0, f32::NAN, f32::INFINITY] {
            let mut node = DrumKitNode::new(SR);
            let (out, _) = render(
                &mut node,
                4_800,
                256,
                &[NoteEvent::note_on(0, SNARE_KEY, velocity)],
            );
            assert!(out.iter().all(|s| s.is_finite()), "velocity {velocity}");
            assert!(peak(&out) <= 1.0, "velocity {velocity} escaped the range");
        }
    }

    #[test]
    fn a_retrigger_restarts_the_same_voice_instead_of_stacking() {
        // Two kicks 10 ms apart: the second must sound at full amplitude, which
        // it cannot do if it stacked on top of a decaying first.
        let mut node = DrumKitNode::new(SR);
        let (first, _) = render(&mut node, 480, 480, &[NoteEvent::note_on(0, KICK_KEY, 1.0)]);
        let (second, _) = render(&mut node, 480, 480, &[NoteEvent::note_on(0, KICK_KEY, 1.0)]);
        assert_eq!(node.active_voices(), 1, "one drum, one voice");

        // The restarted hit begins from the top of its envelope again.
        assert!(
            second[1..64]
                .iter()
                .map(|s| s.abs())
                .fold(0.0_f32, f32::max)
                > first[1..64].iter().map(|s| s.abs()).fold(0.0_f32, f32::max) * 0.8,
            "the retrigger did not restart the envelope"
        );
    }

    #[test]
    fn a_closed_hat_chokes_an_open_one() {
        let mut node = DrumKitNode::new(SR);
        // Open hat, let it ring 50 ms, then close.
        let (ringing, _) = render(
            &mut node,
            2_400,
            256,
            &[NoteEvent::note_on(0, OPEN_HAT_KEY, 1.0)],
        );
        assert!(node.is_sounding(Drum::OpenHat));
        assert!(
            peak(&ringing[1_200..]) > 0.01,
            "the open hat should still ring"
        );

        // 10 ms after the choke, the open hat must be gone. The closed hat is
        // still sounding, so this measures the open voice specifically by
        // asking the node rather than by looking at the mix.
        render(
            &mut node,
            480,
            480,
            &[NoteEvent::note_on(0, CLOSED_HAT_KEY, 1.0)],
        );
        assert!(
            !node.is_sounding(Drum::OpenHat),
            "a closed hat must choke the open one within 10 ms"
        );
        assert!(
            node.is_sounding(Drum::ClosedHat),
            "the closed hat itself plays"
        );
    }

    #[test]
    fn a_choke_removes_the_open_hat_from_the_mix_within_ten_milliseconds() {
        // The same claim, measured in the signal rather than in the flags: an
        // open hat alone must fall to near-silence 10 ms after the choke, while
        // an unchoked one at the same point is still clearly ringing.
        let mut choked = DrumKitNode::new(SR);
        render(
            &mut choked,
            2_400,
            256,
            &[NoteEvent::note_on(0, OPEN_HAT_KEY, 1.0)],
        );
        // Choke with a *zero-velocity* closed hat so only the open hat's decay
        // shows up in the output.
        let (after, _) = render(
            &mut choked,
            480,
            480,
            &[NoteEvent::note_on(0, CLOSED_HAT_KEY, 0.0)],
        );

        let mut ringing = DrumKitNode::new(SR);
        render(
            &mut ringing,
            2_400,
            256,
            &[NoteEvent::note_on(0, OPEN_HAT_KEY, 1.0)],
        );
        let (unchoked, _) = render(&mut ringing, 480, 480, &[]);

        // Compare the last millisecond of each 10 ms window.
        let choked_tail = peak(&after[432..]);
        let ringing_tail = peak(&unchoked[432..]);
        assert!(
            choked_tail < ringing_tail * 0.01,
            "10 ms after the choke the open hat was still at {choked_tail} \
             against an unchoked {ringing_tail}"
        );
    }

    #[test]
    fn a_key_this_kit_has_no_drum_for_is_counted_rather_than_guessed_at() {
        let mut node = DrumKitNode::new(SR);
        let (out, _) = render(
            &mut node,
            4_800,
            256,
            &[
                NoteEvent::note_on(0, 60, 1.0),
                NoteEvent::note_on(0, 37, 1.0),
                NoteEvent::note_off(0, 36),
            ],
        );
        assert!(
            out.iter().all(|s| *s == 0.0),
            "an unmapped key makes no sound"
        );
        assert_eq!(node.unmapped_notes(), 2, "note-offs are not unmapped notes");
        assert_eq!(node.active_voices(), 0);
    }

    #[test]
    fn a_hit_lands_at_its_frame_offset_and_not_at_the_start_of_the_block() {
        let mut node = DrumKitNode::new(SR);
        let (out, _) = render(
            &mut node,
            512,
            512,
            &[NoteEvent::note_on(256, SNARE_KEY, 1.0)],
        );
        assert!(
            out[..256].iter().all(|s| *s == 0.0),
            "nothing may sound before the event's frame"
        );
        assert!(
            peak(&out[256..]) > 0.05,
            "the hit is missing after its frame"
        );
    }

    #[test]
    fn the_factory_is_registered_under_its_type_id() {
        let factory = DrumKitFactory::new();
        assert_eq!(factory.type_id(), "drum-kit");
        let mut node = factory.create(SR, 512);
        let (out, _) = render(
            node.as_mut(),
            4_800,
            512,
            &[NoteEvent::note_on(0, KICK_KEY, 1.0)],
        );
        assert!(peak(&out) > 0.1);
    }
}
