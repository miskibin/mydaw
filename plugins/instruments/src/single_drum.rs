//! One drum, one instrument, one channel.
//!
//! ```text
//!   type_id       instrument     voice            key means
//!   ----------    -----------    -------------    -----------------------------
//!   kick          Kick           KickVoice        pitch, +/- 12 semitones
//!   snare         Snare          SnareVoice       body-tone pitch, +/- 12
//!   hat-closed    Hat Closed     HatVoice         nothing (a hat has no pitch)
//!   hat-open      Hat Open       HatVoice         nothing
//! ```
//!
//! # Why this exists when [`DrumKitNode`](crate::DrumKitNode) already does
//! A rack channel is **one instrument**. That is not a stylistic preference; it
//! is what every other part of the model assumes. A channel has one mixer strip,
//! one fader, one pan, one insert chain and one row in the pattern -- and a
//! "drum kit" channel has four drums behind all of them. You cannot turn the
//! snare down, you cannot pan the hats, you cannot put a different reverb on the
//! kick, and the pattern row for that channel has to carry a key per hit to say
//! *which* drum, which is the one thing a step grid is not shaped to express.
//!
//! So the default project has four drum channels rather than one kit channel,
//! and each of them is a single-voice instrument that answers to **any** key.
//! The kit is kept -- a project saved before this change names `drum-kit`, and a
//! project file that no longer opens is worse than an instrument nobody
//! chooses -- but nothing creates one any more.
//!
//! # Key means pitch, not selection
//! Inside the kit a key chooses a drum, so it cannot also mean a pitch. Here
//! there is nothing to choose, so the key is free to mean what it means
//! everywhere else in the DAW. The kick and the snare read it as a transposition
//! from their own base key ([`KICK_KEY`](crate::KICK_KEY),
//! [`SNARE_KEY`](crate::SNARE_KEY)), clamped to [`PITCH_RANGE_SEMITONES`] either
//! way -- an octave down is a sub kick, an octave up is a tom, and everything in
//! between is a tuning knob you already have a piano roll for. Clamped rather
//! than unbounded because a kick at key 108 is 64 times its own frequency, which
//! is not a drum, and because a step row entered at a stray key should sound
//! wrong rather than inaudible.
//!
//! The hats ignore the key entirely. A hi-hat is filtered noise; "pitching" it
//! would move a highpass corner and sound like a mistake.
//!
//! # No cross-channel choke, yet
//! Inside the kit, a closed hat chokes the open one, because they are one
//! instrument and a real hi-hat cannot be open and closed at once. Across
//! *channels* that would mean one instrument reaching into another's voice
//! state, which needs a routing concept the engine does not have (FL calls it a
//! cut group). Two separate hat channels therefore ring independently. This is
//! a known, deliberate gap; see the spec's limitations.
//!
//! # RT safety
//! [`SingleDrumNode::process`] is arithmetic over a fixed-size struct with one
//! voice in it. No allocation, no locks, no syscalls; the same
//! `tests/rt_alloc.rs` harness that measures the kit measures these.

use mydaw_rt_core::{NoteEvent, NoteKind, ProcessCtx, RtNode, RtNodeFactory};

use crate::drum_kit::{
    ClapVoice, HatVoice, KickVoice, SnareVoice, CLAP_KEY, CLOSED_HAT_DECAY_MS, CLOSED_HAT_KEY,
    CLOSED_HAT_SEED, HAT_GAIN, KICK_KEY, OPEN_HAT_DECAY_MS, OPEN_HAT_KEY, OPEN_HAT_SEED, SNARE_KEY,
};
use crate::dsp::usable_sample_rate;

/// Node type id of the kick instrument. Stable: project files store it.
pub const KICK_TYPE_ID: &str = "kick";
/// Node type id of the snare instrument.
pub const SNARE_TYPE_ID: &str = "snare";
/// Node type id of the closed hi-hat instrument.
pub const CLOSED_HAT_TYPE_ID: &str = "hat-closed";
/// Node type id of the open hi-hat instrument.
pub const OPEN_HAT_TYPE_ID: &str = "hat-open";
/// Node type id of the hand-clap instrument.
pub const CLAP_TYPE_ID: &str = "clap";

/// How far a key may detune a pitched drum, in semitones either way.
///
/// An octave. See the module docs: far enough to be a musical control, near
/// enough that a wrong key sounds wrong rather than disappearing.
pub const PITCH_RANGE_SEMITONES: i32 = 12;

/// Which single drum an instrument is.
///
/// Deliberately not [`Drum`](crate::Drum). That enum answers "which drum does
/// this *key* address inside a kit", which is exactly the question this module
/// exists to stop asking; sharing the type would invite the key-to-drum lookup
/// back in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SingleDrum {
    Kick,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
}

impl SingleDrum {
    /// Every per-drum instrument, in the order a fresh project racks them.
    ///
    /// Kick, snare, clap, hats: the order a drum machine's rows are laid out,
    /// and the order anyone reaches for them in.
    pub const ALL: [Self; 5] = [
        Self::Kick,
        Self::Snare,
        Self::Clap,
        Self::ClosedHat,
        Self::OpenHat,
    ];

    /// The node factory's `type_id`.
    pub const fn type_id(self) -> &'static str {
        match self {
            Self::Kick => KICK_TYPE_ID,
            Self::Snare => SNARE_TYPE_ID,
            Self::Clap => CLAP_TYPE_ID,
            Self::ClosedHat => CLOSED_HAT_TYPE_ID,
            Self::OpenHat => OPEN_HAT_TYPE_ID,
        }
    }

    /// Name for a menu and for the channel a fresh project creates.
    ///
    /// Honest names. The drums are synthesised from oscillators and noise; a
    /// label like "Kick 909" would be claiming a machine this is not.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Kick => "Kick",
            Self::Snare => "Snare",
            Self::Clap => "Clap",
            Self::ClosedHat => "Hat Closed",
            Self::OpenHat => "Hat Open",
        }
    }

    /// One line for a tooltip.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Kick => {
                "Synthesised kick: 420 -> 50 Hz two-stage sweep, punch envelope, soft clip, tunable"
            }
            Self::Snare => {
                "Synthesised snare: a crack over a wire buzz, two body modes at 180 Hz, tunable"
            }
            Self::Clap => "Synthesised clap: three offset noise bursts into a room tail",
            Self::ClosedHat => "Synthesised closed hi-hat: six-oscillator metal, 60 ms decay",
            Self::OpenHat => "Synthesised open hi-hat: six-oscillator metal, 350 ms decay",
        }
    }

    /// The key at which this drum sounds untransposed.
    ///
    /// Its General MIDI number, so a pattern written for the kit plays this
    /// instrument at its natural pitch without editing.
    pub const fn base_key(self) -> u8 {
        match self {
            Self::Kick => KICK_KEY,
            Self::Snare => SNARE_KEY,
            Self::Clap => CLAP_KEY,
            Self::ClosedHat => CLOSED_HAT_KEY,
            Self::OpenHat => OPEN_HAT_KEY,
        }
    }

    /// Whether a note key transposes this drum.
    pub const fn is_pitched(self) -> bool {
        matches!(self, Self::Kick | Self::Snare)
    }
}

/// Frequency multiplier for playing `drum` at `key`.
///
/// `1.0` for the hats and for a key at the drum's base; otherwise
/// `2^(clamped semitones / 12)`. Split out of the node so the clamp is testable
/// as arithmetic rather than through a spectrum.
pub fn pitch_ratio(drum: SingleDrum, key: u8) -> f32 {
    if !drum.is_pitched() {
        return 1.0;
    }
    let offset = i32::from(key) - i32::from(drum.base_key());
    let clamped = offset.clamp(-PITCH_RANGE_SEMITONES, PITCH_RANGE_SEMITONES);
    2.0_f32.powf(clamped as f32 / 12.0)
}

/// The one voice a single-drum instrument owns.
///
/// An enum rather than a trait object: there are four of these, they are all in
/// this crate, and a `Box<dyn Voice>` on the audio thread would buy dynamic
/// dispatch in exchange for nothing.
#[derive(Debug, Clone, Copy)]
enum Voice {
    Kick(KickVoice),
    Snare(SnareVoice),
    Clap(ClapVoice),
    Hat(HatVoice),
}

/// One drum as a whole instrument.
///
/// A **generator**: it overwrites the bus rather than summing into it, exactly
/// like [`DrumKitNode`](crate::DrumKitNode), because it sits at the head of a
/// track chain.
pub struct SingleDrumNode {
    drum: SingleDrum,
    voice: Voice,
    /// Note-ons received. Diagnostics: unlike the kit there is no such thing as
    /// an unmapped key here, so the useful counter is the opposite one -- a
    /// silent channel with a rising count means the instrument is being played
    /// and something downstream is wrong.
    triggers: u64,
}

impl SingleDrumNode {
    /// Build one drum for `sample_rate`. **Non-RT**: every coefficient is
    /// derived here.
    pub fn new(drum: SingleDrum, sample_rate: f64) -> Self {
        let sample_rate = usable_sample_rate(sample_rate);
        let voice = match drum {
            SingleDrum::Kick => Voice::Kick(KickVoice::new(sample_rate)),
            SingleDrum::Snare => Voice::Snare(SnareVoice::new(sample_rate)),
            SingleDrum::Clap => Voice::Clap(ClapVoice::new(sample_rate)),
            SingleDrum::ClosedHat => Voice::Hat(HatVoice::new(
                CLOSED_HAT_DECAY_MS,
                CLOSED_HAT_SEED,
                sample_rate,
            )),
            SingleDrum::OpenHat => {
                Voice::Hat(HatVoice::new(OPEN_HAT_DECAY_MS, OPEN_HAT_SEED, sample_rate))
            }
        };
        Self {
            drum,
            voice,
            triggers: 0,
        }
    }

    /// Which drum this is.
    pub const fn drum(&self) -> SingleDrum {
        self.drum
    }

    /// Whether the voice is sounding.
    pub const fn is_sounding(&self) -> bool {
        match &self.voice {
            Voice::Kick(voice) => voice.is_active(),
            Voice::Snare(voice) => voice.is_active(),
            Voice::Clap(voice) => voice.is_active(),
            Voice::Hat(voice) => voice.is_active(),
        }
    }

    /// Note-ons this instrument has received.
    pub const fn triggers(&self) -> u64 {
        self.triggers
    }

    /// Hit the drum at `key`. Velocity is clamped rather than trusted: this is
    /// reachable from a hosted plugin's note output as well as from our own
    /// validated commands.
    pub fn trigger(&mut self, key: u8, velocity: f32) {
        let velocity = if velocity.is_finite() {
            velocity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let ratio = pitch_ratio(self.drum, key);
        self.triggers = self.triggers.saturating_add(1);
        match &mut self.voice {
            Voice::Kick(voice) => voice.trigger_pitched(velocity, ratio),
            Voice::Snare(voice) => voice.trigger_pitched(velocity, ratio),
            Voice::Clap(voice) => voice.trigger(velocity),
            Voice::Hat(voice) => voice.trigger(velocity),
        }
    }

    /// Apply one event. Note-offs are ignored: a drum is a one-shot, and its
    /// length is a property of the drum rather than of how long a pad was held.
    fn apply(&mut self, event: &NoteEvent) {
        match event.kind {
            NoteKind::NoteOn { key, velocity } => self.trigger(key, velocity),
            NoteKind::NoteOff { .. } => {}
            // `NoteKind` is `#[non_exhaustive]`; a choke or expression event
            // arriving here is not something this instrument claims to know.
            _ => {}
        }
    }

    fn render(&mut self, span: &mut [f32]) {
        match &mut self.voice {
            Voice::Kick(voice) => voice.render(span),
            Voice::Snare(voice) => voice.render(span),
            Voice::Clap(voice) => voice.render(span),
            Voice::Hat(voice) => voice.render(span, HAT_GAIN),
        }
    }
}

impl RtNode for SingleDrumNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        let events = ctx.events;
        let frames = ctx.frames;
        let [left, right] = ctx.buffers.as_mut_slices();
        left.fill(0.0);

        // Split the block at every event offset, exactly as the kit does, so a
        // hit at frame `n` is heard at frame `n`.
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

        right.copy_from_slice(left);
    }
}

/// Builds [`SingleDrumNode`]s for one drum.
#[derive(Debug, Clone, Copy)]
pub struct SingleDrumFactory {
    drum: SingleDrum,
}

impl SingleDrumFactory {
    pub const fn new(drum: SingleDrum) -> Self {
        Self { drum }
    }

    /// The drum this factory builds.
    pub const fn drum(&self) -> SingleDrum {
        self.drum
    }
}

impl RtNodeFactory for SingleDrumFactory {
    fn type_id(&self) -> &'static str {
        self.drum.type_id()
    }

    fn create(&self, sample_rate: f64, _max_block: usize) -> Box<dyn RtNode> {
        // `max_block` is irrelevant: one voice holds no per-block state and
        // writes only into the caller's buffers.
        Box::new(SingleDrumNode::new(self.drum, sample_rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{peak, render};

    const SR: f64 = 48_000.0;

    #[test]
    fn the_type_ids_are_the_documented_ones_and_are_all_different() {
        let ids: Vec<&str> = SingleDrum::ALL.iter().map(|d| d.type_id()).collect();
        assert_eq!(ids, vec!["kick", "snare", "clap", "hat-closed", "hat-open"]);
        // Project files store these; a collision with the kit or the synth would
        // silently load the wrong instrument.
        for id in &ids {
            assert_ne!(*id, crate::drum_kit::NODE_TYPE_ID);
            assert_ne!(*id, crate::poly_synth::NODE_TYPE_ID);
        }
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), SingleDrum::ALL.len());
    }

    #[test]
    fn the_names_describe_what_the_dsp_actually_is() {
        // The design mock labels these "Kick 909" and "Clap 808". These are
        // oscillators and noise, and calling them a 909 would be a claim about
        // hardware nobody sampled.
        assert_eq!(SingleDrum::Kick.display_name(), "Kick");
        assert_eq!(SingleDrum::Snare.display_name(), "Snare");
        assert_eq!(SingleDrum::ClosedHat.display_name(), "Hat Closed");
        assert_eq!(SingleDrum::OpenHat.display_name(), "Hat Open");
        for drum in SingleDrum::ALL {
            assert!(!drum.description().is_empty());
            assert!(
                !drum.display_name().contains("909") && !drum.display_name().contains("808"),
                "{}",
                drum.display_name()
            );
        }
    }

    #[test]
    fn the_base_keys_are_the_general_midi_ones_the_kit_uses() {
        // So a pattern written against the kit plays these at their own pitch.
        assert_eq!(SingleDrum::Kick.base_key(), 36);
        assert_eq!(SingleDrum::Snare.base_key(), 38);
        assert_eq!(SingleDrum::ClosedHat.base_key(), 42);
        assert_eq!(SingleDrum::OpenHat.base_key(), 46);
    }

    #[test]
    fn any_key_at_all_triggers_the_drum() {
        // The whole point of a per-drum instrument: there is nothing to select,
        // so nothing to miss. The kit counts unmapped notes; this cannot have any.
        for drum in SingleDrum::ALL {
            for key in [0_u8, 36, 60, 100, 127] {
                let mut node = SingleDrumNode::new(drum, SR);
                let (out, _) = render(&mut node, 9_600, 256, &[NoteEvent::note_on(0, key, 1.0)]);
                assert!(
                    peak(&out) > 0.05,
                    "{} was silent at key {key} (peak {})",
                    drum.display_name(),
                    peak(&out)
                );
                assert_eq!(node.triggers(), 1);
            }
        }
    }

    #[test]
    fn an_untriggered_drum_is_digitally_silent_and_stereo_matches() {
        for drum in SingleDrum::ALL {
            let mut node = SingleDrumNode::new(drum, SR);
            let (left, right) = render(&mut node, 4_800, 128, &[]);
            assert!(left.iter().all(|s| *s == 0.0), "{drum:?} made a sound");
            assert_eq!(left, right, "{drum:?} is mono fanned to stereo");
            assert!(!node.is_sounding());
        }
    }

    #[test]
    fn the_pitch_ratio_is_an_octave_either_way_and_clamps_beyond_it() {
        let base = SingleDrum::Kick.base_key();
        assert_eq!(pitch_ratio(SingleDrum::Kick, base), 1.0);
        // Twelve semitones up doubles, twelve down halves.
        assert!((pitch_ratio(SingleDrum::Kick, base + 12) - 2.0).abs() < 1e-6);
        assert!((pitch_ratio(SingleDrum::Kick, base - 12) - 0.5).abs() < 1e-6);
        // And beyond the range it stays there rather than running away.
        assert!((pitch_ratio(SingleDrum::Kick, 127) - 2.0).abs() < 1e-6);
        assert!((pitch_ratio(SingleDrum::Kick, 0) - 0.5).abs() < 1e-6);
        // A semitone is the twelfth root of two, so seven of them is a fifth.
        let fifth = pitch_ratio(SingleDrum::Kick, base + 7);
        assert!((fifth - 1.498_307).abs() < 1e-4, "{fifth}");

        // The snare transposes from its own base, not the kick's.
        assert_eq!(pitch_ratio(SingleDrum::Snare, SNARE_KEY), 1.0);
        assert!(pitch_ratio(SingleDrum::Snare, KICK_KEY) < 1.0);

        // The hats do not transpose at all.
        for hat in [SingleDrum::ClosedHat, SingleDrum::OpenHat] {
            for key in [0_u8, 42, 60, 127] {
                assert_eq!(pitch_ratio(hat, key), 1.0, "{hat:?} at {key}");
            }
        }
    }

    /// The ratio has to reach the *sound*, not only the arithmetic. A kick an
    /// octave up crosses zero twice as often, which is a claim about the samples
    /// that no amount of unit-testing `pitch_ratio` would make.
    #[test]
    fn a_kick_an_octave_up_oscillates_twice_as_fast() {
        let zero_crossings = |key: u8| {
            let mut node = SingleDrumNode::new(SingleDrum::Kick, SR);
            // From 20 ms in, so the sweep has settled and the noise click is
            // long gone; the body tone is all that is left.
            let (out, _) = render(&mut node, 12_000, 256, &[NoteEvent::note_on(0, key, 1.0)]);
            out[960..9_600]
                .windows(2)
                .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
                .count()
        };
        let base = zero_crossings(KICK_KEY);
        let octave = zero_crossings(KICK_KEY + 12);
        let ratio = octave as f64 / base as f64;
        assert!(
            (ratio - 2.0).abs() < 0.1,
            "an octave up should double the frequency, got {ratio} ({base} -> {octave})"
        );
    }

    /// Same claim for the snare's body tone, which is the only pitched part of
    /// it. Measured through a lowpass, because the snare is mostly noise
    /// highpassed at 1 kHz and counting raw zero crossings would be counting
    /// the wires rather than the drum.
    #[test]
    fn a_snare_an_octave_down_has_a_lower_body_tone() {
        let body_crossings = |key: u8| {
            let mut node = SingleDrumNode::new(SingleDrum::Snare, SR);
            let (out, _) = render(&mut node, 12_000, 256, &[NoteEvent::note_on(0, key, 1.0)]);
            // 400 Hz: well above the 180 Hz body and well below the 1 kHz
            // corner the wires sit above.
            let mut lowpass = crate::OnePole::new(400.0, SR as f32);
            let body: Vec<f32> = out.iter().map(|s| lowpass.low(*s)).collect();
            // From 5 ms in, so the filter has settled past its own transient.
            body[240..4_800]
                .windows(2)
                .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
                .count()
        };
        let base = body_crossings(SNARE_KEY);
        let down = body_crossings(SNARE_KEY - 12);
        let up = body_crossings(SNARE_KEY + 12);
        assert!(
            down < base && base < up,
            "the body tone must follow the key: {down} (down) < {base} (base) < {up} (up)"
        );
    }

    #[test]
    fn a_hit_stays_below_full_scale_and_velocity_scales_it() {
        for drum in SingleDrum::ALL {
            let mut loud = SingleDrumNode::new(drum, SR);
            let (full, _) = render(
                &mut loud,
                24_000,
                256,
                &[NoteEvent::note_on(0, drum.base_key(), 1.0)],
            );
            let full_peak = peak(&full);
            assert!(
                (0.1..1.0).contains(&full_peak),
                "{} peaked at {full_peak}",
                drum.display_name()
            );

            let mut quiet = SingleDrumNode::new(drum, SR);
            let (half, _) = render(
                &mut quiet,
                24_000,
                256,
                &[NoteEvent::note_on(0, drum.base_key(), 0.5)],
            );
            let ratio = peak(&half) / full_peak;
            assert!(
                (ratio - 0.5).abs() < 1e-4,
                "{}: half velocity gave {ratio} of the amplitude",
                drum.display_name()
            );
        }
    }

    #[test]
    fn a_velocity_the_engine_should_never_send_is_clamped_rather_than_trusted() {
        for velocity in [-1.0_f32, 4.0, f32::NAN, f32::INFINITY] {
            for drum in SingleDrum::ALL {
                let mut node = SingleDrumNode::new(drum, SR);
                let (out, _) = render(
                    &mut node,
                    4_800,
                    256,
                    &[NoteEvent::note_on(0, drum.base_key(), velocity)],
                );
                assert!(out.iter().all(|s| s.is_finite()), "{drum:?} / {velocity}");
                assert!(peak(&out) <= 1.0, "{drum:?} / {velocity} escaped the range");
            }
        }
    }

    #[test]
    fn a_hit_lands_at_its_frame_offset_and_not_at_the_start_of_the_block() {
        let mut node = SingleDrumNode::new(SingleDrum::Snare, SR);
        let (out, _) = render(&mut node, 512, 512, &[NoteEvent::note_on(256, 60, 1.0)]);
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
    fn a_retrigger_restarts_the_one_voice_rather_than_stacking() {
        let mut node = SingleDrumNode::new(SingleDrum::Kick, SR);
        let (first, _) = render(&mut node, 480, 480, &[NoteEvent::note_on(0, 36, 1.0)]);
        let (second, _) = render(&mut node, 480, 480, &[NoteEvent::note_on(0, 36, 1.0)]);
        assert_eq!(node.triggers(), 2);
        assert!(
            peak(&second[1..64]) > peak(&first[1..64]) * 0.8,
            "the retrigger did not restart the envelope"
        );
    }

    /// A note-off must not silence a one-shot: the length of a kick is a
    /// property of the kick, not of how long the key was held.
    #[test]
    fn a_note_off_does_not_cut_the_drum_short() {
        let mut node = SingleDrumNode::new(SingleDrum::OpenHat, SR);
        let (out, _) = render(
            &mut node,
            4_800,
            256,
            &[NoteEvent::note_on(0, 46, 1.0), NoteEvent::note_off(128, 46)],
        );
        assert!(node.is_sounding(), "the open hat rings past its note-off");
        assert!(peak(&out[2_400..]) > 0.001, "and is still audible");
    }

    /// The gap the module docs promise to be honest about: two hat *channels*
    /// do not choke each other, because they are two instruments.
    #[test]
    fn a_closed_hat_channel_does_not_choke_a_separate_open_hat_channel() {
        let mut open = SingleDrumNode::new(SingleDrum::OpenHat, SR);
        render(&mut open, 2_400, 256, &[NoteEvent::note_on(0, 46, 1.0)]);
        assert!(open.is_sounding());

        // A closed hat on its own channel. Nothing connects the two, and the
        // open hat carries on -- which is the documented limitation, asserted
        // so that adding cut groups later has a test to flip.
        let mut closed = SingleDrumNode::new(SingleDrum::ClosedHat, SR);
        render(&mut closed, 480, 480, &[NoteEvent::note_on(0, 42, 1.0)]);
        render(&mut open, 480, 480, &[]);
        assert!(
            open.is_sounding(),
            "cross-channel choke is not implemented; if this fails, update the docs"
        );
    }

    #[test]
    fn each_factory_builds_its_own_drum() {
        for drum in SingleDrum::ALL {
            let factory = SingleDrumFactory::new(drum);
            assert_eq!(factory.type_id(), drum.type_id());
            assert_eq!(factory.drum(), drum);
            let mut node = factory.create(SR, 512);
            let (out, _) = render(
                node.as_mut(),
                9_600,
                512,
                &[NoteEvent::note_on(0, drum.base_key(), 1.0)],
            );
            assert!(peak(&out) > 0.05, "{}", drum.display_name());
        }
    }

    /// The kit is unchanged by the refactor that made its voices shareable.
    #[test]
    fn the_kick_at_its_base_key_is_bit_identical_to_the_kits_kick() {
        let mut single = SingleDrumNode::new(SingleDrum::Kick, SR);
        let (from_single, _) = render(&mut single, 12_000, 256, &[NoteEvent::note_on(0, 36, 1.0)]);

        let mut kit = crate::DrumKitNode::new(SR);
        let (from_kit, _) = render(&mut kit, 12_000, 256, &[NoteEvent::note_on(0, 36, 1.0)]);

        assert_eq!(
            from_single, from_kit,
            "an unpitched kick must be the same samples either way"
        );
    }
}
