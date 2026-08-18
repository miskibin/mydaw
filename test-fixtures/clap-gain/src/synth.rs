//! `dev.mydaw.fixture.synth` -- the note-event test vehicle.
//!
//! Step 2 of the roadmap carries note events from the UI, through the RT graph,
//! into a CLAP plugin. Proving that end to end needs a plugin that *makes a
//! sound only when it is told to*, and whose sound is predictable enough to
//! assert on. So the workspace builds one, in the same bundle as the gain
//! fixture.
//!
//! # The DSP, and why it is this boring
//! Up to [`VOICES`] sine voices. A note-on takes a free voice, sets its
//! frequency from the key by equal temperament ([`key_to_hz`], A4 = key 69 =
//! 440 Hz), sets its target amplitude to `velocity * `[`VOICE_GAIN`], and starts
//! its phase at zero. A note-off releases the voice over [`RELEASE_MS`]. Every
//! voice starts with a [`ATTACK_MS`] ramp, because a sine switched on at full
//! amplitude clicks, and a click is broadband -- it would smear the spectrum a
//! test is trying to measure.
//!
//! Everything is deterministic: no noise, no drift, no free-running LFO. The
//! same events at the same offsets produce the same samples, bit for bit, which
//! is what lets `plugins/host-clap` and the shell's capture tests assert real
//! numbers (`dominant_hz ~= 440`, `peak ~= velocity * VOICE_GAIN`) instead of
//! "something happened".
//!
//! # Ports
//! No audio input, one stereo audio output, one CLAP-dialect note input. That
//! combination is the point: it is the shape a host has to special-case, since
//! an instrument cannot be handed the input buffers an effect gets.
//!
//! # Sample accuracy
//! [`SynthProcessor::process`] splits the block at every event offset and
//! renders each span separately, so an event at frame `n` is heard at frame `n`
//! and not at the start of the block. Live input always arrives at offset 0
//! today, but the sequencer will not, and a fixture that ignored offsets could
//! not tell the difference.

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::events::Match;
use clack_plugin::plugin::PluginError;
use clack_plugin::prelude::*;

/// CLAP id of the synth fixture.
pub const PLUGIN_ID: &str = "dev.mydaw.fixture.synth";

/// Display name reported in the plugin descriptor.
pub const PLUGIN_NAME: &str = "MYDAW Fixture Synth";

/// Voices this synth can sound at once.
///
/// Eight is enough for a two-handed chord and small enough that the whole voice
/// array fits in a cache line or two, so `process` stays trivially fast.
pub const VOICES: usize = 8;

/// Amplitude of one voice at velocity 1.0.
///
/// 0.2 (-13.98 dBFS) leaves room for a four-note chord at full velocity without
/// reaching full scale even in the worst case where every partial lines up,
/// which is what a test asserting `peak` needs to be able to predict. It also
/// matches the test tone's default amplitude, so switching a chain between the
/// two does not change the meter's ballpark.
pub const VOICE_GAIN: f32 = 0.2;

/// Attack ramp, in milliseconds.
///
/// Short enough to be inaudible as an attack, long enough to stop the step
/// discontinuity that a hard note-on would otherwise put into the spectrum.
pub const ATTACK_MS: f32 = 2.0;

/// Release ramp, in milliseconds. A note-off is silent this long afterwards.
pub const RELEASE_MS: f32 = 50.0;

/// Reference pitch: key 69 is A4.
pub const A4_KEY: f32 = 69.0;
/// Frequency of [`A4_KEY`], in Hz.
pub const A4_HZ: f32 = 440.0;

/// Equal-tempered frequency of a MIDI key number.
///
/// `f = 440 * 2^((key - 69) / 12)`. Exact at key 69 by construction, which is
/// the case every test uses.
pub fn key_to_hz(key: u8) -> f32 {
    A4_HZ * ((f32::from(key) - A4_KEY) / 12.0).exp2()
}

/// One sine voice.
///
/// The envelope is a single value chasing a target: `target > 0` while the key
/// is held, `0` after the note-off. Reaching `0` while releasing frees the
/// voice. That is two states expressed in one number, and it means a note-on
/// that arrives during a release simply re-targets the envelope instead of
/// needing a state machine.
#[derive(Debug, Clone, Copy)]
struct Voice {
    /// `None` when the voice is free.
    key: Option<u8>,
    channel: u8,
    /// True between note-on and note-off; a released voice keeps its key until
    /// the envelope reaches zero so a note-off cannot free the slot early.
    held: bool,
    phase: f32,
    /// Phase increment per sample, in cycles.
    phase_step: f32,
    envelope: f32,
    target: f32,
    /// Age in note-ons, for stealing the oldest voice when all are busy.
    age: u64,
}

impl Voice {
    const fn free() -> Self {
        Self {
            key: None,
            channel: 0,
            held: false,
            phase: 0.0,
            phase_step: 0.0,
            envelope: 0.0,
            target: 0.0,
            age: 0,
        }
    }

    const fn is_active(&self) -> bool {
        self.key.is_some()
    }
}

/// The synth plugin type.
pub struct SynthPlugin;

impl Plugin for SynthPlugin {
    type AudioProcessor<'a> = SynthProcessor;
    type Shared<'a> = ();
    type MainThread<'a> = SynthMainThread;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&()>) {
        builder.register::<PluginAudioPorts>();
        // Without this a CLAP plugin formally has no note ports, and a correct
        // host will not send it a single event. Declaring it is what makes this
        // fixture a real subject for the host's note dispatch.
        builder.register::<PluginNotePorts>();
    }
}

/// Main-thread half: nothing but the port descriptions.
pub struct SynthMainThread;

impl PluginMainThread<'_, ()> for SynthMainThread {}

impl PluginAudioPortsImpl for SynthMainThread {
    /// No audio input, one stereo output. An instrument, in CLAP's terms.
    fn count(&mut self, is_input: bool) -> u32 {
        u32::from(!is_input)
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if is_input || index != 0 {
            return;
        }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: b"Stereo Out",
            channel_count: crate::CHANNELS,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
        });
    }
}

impl PluginNotePortsImpl for SynthMainThread {
    /// One note input, no note output.
    fn count(&mut self, is_input: bool) -> u32 {
        u32::from(is_input)
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if !is_input || index != 0 {
            return;
        }
        writer.set(&NotePortInfo {
            id: ClapId::new(0),
            name: b"Notes",
            // CLAP-dialect only: MYDAW speaks CLAP note events natively, and a
            // fixture that also accepted MIDI would let a host bug (sending
            // MIDI where CLAP was expected) pass unnoticed.
            supported_dialects: NoteDialects::CLAP,
            preferred_dialect: Some(NoteDialect::Clap),
        });
    }
}

/// Audio-thread half: the voices and one scratch buffer.
pub struct SynthProcessor {
    voices: [Voice; VOICES],
    sample_rate: f32,
    /// Envelope steps per sample, derived from the sample rate at activation.
    attack_step: f32,
    release_step: f32,
    /// Mono render buffer, copied to every output channel.
    ///
    /// Rendering mono once and fanning out is both cheaper and simpler than
    /// borrowing two output channels at the same time, and this synth has
    /// nothing stereo to say.
    scratch: Vec<f32>,
    /// Note-ons seen, for voice stealing.
    counter: u64,
}

impl SynthProcessor {
    /// Envelope step per sample for a ramp of `ms` milliseconds.
    fn step_for(ms: f32, sample_rate: f32) -> f32 {
        let frames = (ms / 1000.0) * sample_rate;
        if frames >= 1.0 {
            1.0 / frames
        } else {
            1.0
        }
    }

    /// Start (or re-trigger) a voice for `key`.
    fn note_on(&mut self, channel: u8, key: u8, velocity: f32) {
        self.counter = self.counter.wrapping_add(1);
        let index = self.allocate(channel, key);
        let voice = &mut self.voices[index];
        // Retriggering a still-sounding voice keeps its envelope, so a repeated
        // key does not click; a fresh voice starts from silence and ramps.
        if voice.key != Some(key) || !voice.is_active() {
            voice.envelope = 0.0;
            voice.phase = 0.0;
        }
        voice.key = Some(key);
        voice.channel = channel;
        voice.held = true;
        voice.phase_step = key_to_hz(key) / self.sample_rate;
        voice.target = velocity.clamp(0.0, 1.0) * VOICE_GAIN;
        voice.age = self.counter;
    }

    /// Release every voice matching `channel` and `key`.
    fn note_off(&mut self, channel: u8, key: u8) {
        for voice in &mut self.voices {
            if voice.held && voice.key == Some(key) && voice.channel == channel {
                voice.held = false;
                voice.target = 0.0;
            }
        }
    }

    /// Pick the slot a new note should use: the same key if it is still
    /// sounding, else a free slot, else the oldest voice.
    fn allocate(&self, channel: u8, key: u8) -> usize {
        if let Some(index) = self
            .voices
            .iter()
            .position(|v| v.is_active() && v.key == Some(key) && v.channel == channel)
        {
            return index;
        }
        if let Some(index) = self.voices.iter().position(|v| !v.is_active()) {
            return index;
        }
        // All eight busy: steal the oldest. Dropping the note instead would make
        // a stuck-voice bug look like a missing-note bug.
        self.voices
            .iter()
            .enumerate()
            .min_by_key(|(_, v)| v.age)
            .map_or(0, |(index, _)| index)
    }

    /// Render `frames` samples of every active voice into the scratch buffer,
    /// starting at `start`. Additive; the caller has already zeroed the span.
    fn render(&mut self, start: usize, frames: usize) {
        if frames == 0 {
            return;
        }
        let span = &mut self.scratch[start..start + frames];
        for voice in &mut self.voices {
            if !voice.is_active() {
                continue;
            }
            for sample in span.iter_mut() {
                // Envelope first, so a voice released exactly at this frame
                // starts falling here rather than one frame late.
                if voice.envelope < voice.target {
                    voice.envelope = (voice.envelope + self.attack_step).min(voice.target);
                } else if voice.envelope > voice.target {
                    voice.envelope = (voice.envelope - self.release_step).max(voice.target);
                }
                *sample += (voice.phase * std::f32::consts::TAU).sin() * voice.envelope;
                voice.phase += voice.phase_step;
                if voice.phase >= 1.0 {
                    voice.phase -= 1.0;
                }
            }
            if !voice.held && voice.envelope <= 0.0 {
                *voice = Voice::free();
            }
        }
    }

    /// Voices currently sounding or releasing. Diagnostics and tests.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.is_active()).count()
    }
}

impl<'a> PluginAudioProcessor<'a, (), SynthMainThread> for SynthProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut SynthMainThread,
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sample_rate = audio_config.sample_rate as f32;
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(PluginError::Message("invalid sample rate"));
        }
        Ok(Self {
            voices: [Voice::free(); VOICES],
            sample_rate,
            attack_step: Self::step_for(ATTACK_MS, sample_rate),
            release_step: Self::step_for(RELEASE_MS, sample_rate),
            // The only allocation in this plugin, and it happens here, off the
            // audio thread, exactly like a host's own RT buffers.
            scratch: vec![0.0; audio_config.max_frames_count as usize],
            counter: 0,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let frames = (audio.frames_count() as usize).min(self.scratch.len());
        self.scratch[..frames].fill(0.0);

        // Split the block at each event offset: render up to the event, apply
        // it, carry on. This is what makes the fixture sample-accurate.
        let mut cursor = 0_usize;
        for event in events.input {
            let Some(header) = event.as_core_event() else {
                continue;
            };
            let at = (event.header().time() as usize).min(frames);
            if at > cursor {
                self.render(cursor, at - cursor);
                cursor = at;
            }
            match header {
                CoreEventSpace::NoteOn(note) => self.apply_note_on(note),
                CoreEventSpace::NoteOff(note) => self.apply_note_off(note),
                // Everything else (parameters, transport, expression) is not
                // something this fixture claims to understand.
                _ => {}
            }
        }
        self.render(cursor, frames - cursor);

        let Some(mut port) = audio.output_port(0) else {
            return Ok(ProcessStatus::Continue);
        };
        // 32-bit only, like the gain fixture: a host asking for f64 gets
        // silence rather than a wrong answer.
        let Some(mut channels) = port.channels()?.into_f32() else {
            return Ok(ProcessStatus::Continue);
        };
        for channel in channels.iter_mut() {
            let shared = channel.len().min(frames);
            channel[..shared].copy_from_slice(&self.scratch[..shared]);
            // A block longer than the buffer we were activated for should be
            // impossible; defining the tail beats leaking stale samples.
            channel[shared..].fill(0.0);
        }

        // Always `Continue`, even with no voices sounding: a `Sleep` would let a
        // host stop calling `process`, and then a note-on would arrive at a
        // plugin that is not running. Predictability over cleverness, again.
        Ok(ProcessStatus::Continue)
    }
}

impl SynthProcessor {
    fn apply_note_on(&mut self, note: &NoteOnEvent) {
        let (Some(key), Some(channel)) = (key_of(note.key()), channel_of(note.channel())) else {
            return;
        };
        self.note_on(channel, key, note.velocity() as f32);
    }

    fn apply_note_off(&mut self, note: &NoteOffEvent) {
        let Some(key) = key_of(note.key()) else {
            return;
        };
        match channel_of(note.channel()) {
            Some(channel) => self.note_off(channel, key),
            // A wildcard channel releases the key wherever it is playing.
            None => {
                for channel in 0..16 {
                    self.note_off(channel, key);
                }
            }
        }
    }
}

/// A specific key number, or `None` for CLAP's wildcard.
fn key_of(key: Match<u16>) -> Option<u8> {
    match key {
        Match::Specific(key) => u8::try_from(key).ok(),
        Match::All => None,
    }
}

/// A specific channel, or `None` for CLAP's wildcard.
fn channel_of(channel: Match<u16>) -> Option<u8> {
    match channel {
        Match::Specific(channel) => u8::try_from(channel).ok(),
        Match::All => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_temperament_anchors_on_a440() {
        assert!((key_to_hz(69) - 440.0).abs() < 1e-3);
        assert!((key_to_hz(81) - 880.0).abs() < 1e-2, "an octave up");
        assert!((key_to_hz(57) - 220.0).abs() < 1e-2, "an octave down");
        // The C major triad the capture test plays.
        assert!((key_to_hz(60) - 261.626).abs() < 0.01);
        assert!((key_to_hz(64) - 329.628).abs() < 0.01);
        assert!((key_to_hz(67) - 391.995).abs() < 0.01);
    }

    #[test]
    fn the_envelope_ramps_take_the_advertised_time() {
        let sr = 48_000.0;
        let attack = SynthProcessor::step_for(ATTACK_MS, sr);
        let release = SynthProcessor::step_for(RELEASE_MS, sr);
        assert!(
            (1.0 / attack - 96.0).abs() < 1e-3,
            "2 ms at 48 kHz is 96 frames"
        );
        assert!(
            (1.0 / release - 2_400.0).abs() < 1e-2,
            "50 ms at 48 kHz is 2400 frames"
        );
        // A nonsensical rate must not produce an infinite or zero step.
        assert_eq!(SynthProcessor::step_for(ATTACK_MS, 1.0), 1.0);
    }

    #[test]
    fn a_chord_at_full_velocity_stays_below_full_scale() {
        // Four notes is the practical worst case for the capture tests; eight
        // would clip, which is a documented property of the fixture rather than
        // a bug (a real synth would have a master gain).
        const { assert!(VOICE_GAIN * 4.0 < 1.0) };
        assert_eq!(VOICES, 8);
    }

    #[test]
    fn the_identity_is_distinct_from_the_gain_fixture() {
        assert_ne!(PLUGIN_ID, crate::PLUGIN_ID);
        assert!(PLUGIN_ID.starts_with("dev.mydaw.fixture."));
    }
}
