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
//!
//! # State: `clap.state`, and why it is a detune
//! This plugin implements the CLAP `state` extension, so the host's
//! save/load/preset path has something real to test against. The gain fixture
//! deliberately does *not* -- it is the negative case, the plugin whose
//! `get_extension("clap.state")` comes back null, and the host's "this plugin
//! cannot be saved" branch needs a subject too.
//!
//! The saved value is a **detune in semitones** ([`state_blob`],
//! [`parse_state`]), and that choice is the whole point. A state extension is
//! trivial to test byte-wise -- write eight bytes, read eight bytes back, assert
//! they match -- but a byte-wise test proves only that the host moved a buffer
//! around; it would still pass if the plugin ignored the loaded value entirely.
//! Detune closes that hole: it moves the sounding pitch, and pitch is a spectral
//! peak, so a host test can load a blob, render a note, run an FFT, and assert
//! on the *sound*. Load detune `12.0`, hold key 69, and the dominant bin has to
//! land on 880 Hz instead of 440 Hz. A gain-shaped state value would not do
//! this nearly as well: a wrong gain is easy to confuse with an envelope, a
//! meter's ballistics, or a host applying its own trim, whereas a frequency is
//! either there or it is not.
//!
//! ## Blob layout
//! Eight bytes, fixed:
//!
//! | Offset | Size | Contents |
//! | --- | --- | --- |
//! | 0 | 4 | [`STATE_MAGIC`] (`MFS1`, "MYDAW Fixture Synth, version 1") |
//! | 4 | 4 | detune in semitones, `f32`, little-endian |
//!
//! The magic exists so a host test can assert on the exact bytes and so a blob
//! from some *other* plugin, fed in by a confused host, is rejected loudly
//! instead of being read as a nonsense pitch. Little-endian is not a portability
//! statement, it is a choice: MYDAW targets little-endian machines, and picking
//! an explicit encoding beats `to_ne_bytes` silently meaning two different
//! things. The detune is clamped to ±[`MAX_DETUNE_SEMITONES`] (two octaves) on
//! the way in and on the way out, and a non-finite value is not storable at all
//! -- an infinite `phase_step` would poison the voice array for the rest of the
//! session, and "the fixture went silent forever" is the least debuggable
//! failure a test can have.
//!
//! ## Where the detune is applied
//! [`SynthShared`] holds the value in an atomic, because CLAP's `state.load` is
//! a *main-thread* call while the DSP reads it on the audio thread. The
//! processor re-reads it once per [`SynthProcessor::process`] call, not once per
//! sample, and recomputes every active voice's `phase_step` from its stored
//! `base_hz`. So a load lands on the next block for notes that are *already
//! held* -- which is what a host test wants, since it can start a note, load a
//! preset, and hear the pitch move without having to re-trigger anything.

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent, ParamValueEvent};
use clack_plugin::events::io::{InputEvents, OutputEvents};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::events::Match;
use clack_plugin::plugin::PluginError;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};

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

// ---------------------------------------------------------------------------
// State: the detune blob
// ---------------------------------------------------------------------------

/// First four bytes of every state blob this plugin writes: `MFS1`.
///
/// "MYDAW Fixture Synth, format 1". A host test asserts on these bytes
/// directly, and a blob that does not start with them is rejected rather than
/// reinterpreted -- a wrong-plugin blob is a host bug worth failing on, not a
/// pitch worth guessing at.
pub const STATE_MAGIC: [u8; 4] = *b"MFS1";

/// Largest detune, in semitones, in either direction: two octaves.
///
/// Two octaves is generous enough that a test can pick an obviously audible
/// offset and narrow enough that key 0 detuned down stays above DC and key 127
/// detuned up stays below Nyquist at 44.1 kHz, so no clamp anywhere else in the
/// DSP has to exist to keep the fixture well behaved.
pub const MAX_DETUNE_SEMITONES: f32 = 24.0;

/// Bytes this plugin's state blob always occupies: magic plus one `f32`.
const STATE_LEN: usize = 8;

/// The largest blob [`SynthMainThread::load`] will accept before giving up.
///
/// A host is free to hand the plugin a stream of any length, including a
/// hostile or corrupt one. Reading into a fixed buffer and refusing anything
/// longer keeps `load` allocation-bounded; a fixture that happily read a
/// gigabyte because a host said so would be testing the allocator, not the
/// state extension.
const STATE_READ_CAP: usize = 64;

/// Builds the [`STATE_LEN`]-byte state blob for a detune in semitones.
///
/// The value is clamped to ±[`MAX_DETUNE_SEMITONES`]; a non-finite input
/// becomes `0.0`. Clamping here rather than rejecting means a host that asks
/// for a silly detune still gets a blob it can round-trip, which keeps the
/// failure modes of "bad value" and "bad bytes" from being confused with each
/// other.
pub fn state_blob(detune_semitones: f32) -> Vec<u8> {
    let detune = sanitize_detune(detune_semitones);
    let mut bytes = Vec::with_capacity(STATE_LEN);
    bytes.extend_from_slice(&STATE_MAGIC);
    bytes.extend_from_slice(&detune.to_le_bytes());
    bytes
}

/// Reads a detune in semitones back out of a state blob.
///
/// The inverse of [`state_blob`]. Returns `None` -- never panics, for any
/// input, including empty and truncated slices -- when the length is not
/// [`STATE_LEN`], the magic does not match, or the stored `f32` is non-finite
/// or outside ±[`MAX_DETUNE_SEMITONES`]. Out-of-range is a rejection here and a
/// clamp in [`state_blob`] on purpose: this function's job is to decide whether
/// bytes that arrived from outside are trustworthy, and bytes carrying a value
/// this plugin would never have written are not.
pub fn parse_state(bytes: &[u8]) -> Option<f32> {
    if bytes.len() != STATE_LEN || bytes[..4] != STATE_MAGIC {
        return None;
    }
    let raw: [u8; 4] = bytes[4..8].try_into().ok()?;
    let detune = f32::from_le_bytes(raw);
    if !detune.is_finite() || detune.abs() > MAX_DETUNE_SEMITONES {
        return None;
    }
    Some(detune)
}

/// Frequency multiplier for a detune in semitones: `2^(semitones / 12)`.
///
/// Equal temperament, same as [`key_to_hz`], so a detune of exactly 12 is
/// exactly an octave and a host test can predict the resulting spectral peak
/// with the same arithmetic the plugin used.
pub fn detune_ratio(semitones: f32) -> f32 {
    (semitones / 12.0).exp2()
}

/// A detune the plugin is willing to hold: finite, and within range.
fn sanitize_detune(semitones: f32) -> f32 {
    if semitones.is_finite() {
        semitones.clamp(-MAX_DETUNE_SEMITONES, MAX_DETUNE_SEMITONES)
    } else {
        0.0
    }
}

/// The synth's thread-safe state: the detune, and nothing else.
///
/// CLAP puts `state.load` on the main thread and `process` on the audio thread,
/// so the one value they share has to cross threads. An `AtomicU32` holding the
/// `f32`'s bits is the whole mechanism: it is lock-free, so the audio thread
/// never blocks behind a preset load, and it makes `Shared` [`Send`] + [`Sync`]
/// without a mutex the RT thread would have to promise not to contend on.
///
/// Relaxed ordering throughout: there is exactly one value and nothing is
/// published alongside it, so there is no happens-before relationship to
/// establish. The worst case is that a load lands one block later than it
/// might have, which no test can hear.
#[derive(Debug, Default)]
pub struct SynthShared {
    /// Detune in semitones, as `f32::to_bits`. Zero bits is `0.0`, so
    /// `Default` is the un-detuned plugin without a hand-written impl.
    detune_bits: AtomicU32,
}

impl SynthShared {
    /// The current detune, in semitones. `0.0` until something sets it.
    pub fn detune(&self) -> f32 {
        f32::from_bits(self.detune_bits.load(Ordering::Relaxed))
    }

    /// Sets the detune, clamped to ±[`MAX_DETUNE_SEMITONES`].
    ///
    /// A non-finite argument stores `0.0` rather than propagating: a NaN detune
    /// would give every voice a NaN `phase_step`, and the fixture would go
    /// permanently silent in a way no assertion points at its cause.
    pub fn set_detune(&self, semitones: f32) {
        self.detune_bits
            .store(sanitize_detune(semitones).to_bits(), Ordering::Relaxed);
    }
}

impl PluginShared<'_> for SynthShared {}

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
    /// Un-detuned frequency of the key that started this voice, in Hz.
    ///
    /// Kept rather than folded straight into `phase_step` so that a detune
    /// arriving mid-note can be applied to a voice that is already sounding:
    /// the step is derived from this every block, not once at note-on.
    base_hz: f32,
    /// Phase increment per sample, in cycles. Recomputed once per block from
    /// `base_hz` and the current detune ratio.
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
            base_hz: 0.0,
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
    type AudioProcessor<'a> = SynthProcessor<'a>;
    type Shared<'a> = SynthShared;
    type MainThread<'a> = SynthMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&SynthShared>) {
        builder.register::<PluginAudioPorts>();
        // Without this a CLAP plugin formally has no note ports, and a correct
        // host will not send it a single event. Declaring it is what makes this
        // fixture a real subject for the host's note dispatch.
        builder.register::<PluginNotePorts>();
        // The reason this plugin, and not the gain fixture, carries state: see
        // the module docs. The gain plugin stays state-less so the host's
        // "plugin cannot be saved" path has a subject.
        builder.register::<PluginState>();
        builder.register::<PluginParams>();
    }
}

/// Main-thread half: the port descriptions, and the state extension.
///
/// It borrows [`SynthShared`] because `state.load` arrives here, on the main
/// thread, while the value it delivers is read by the audio thread.
pub struct SynthMainThread<'a> {
    shared: &'a SynthShared,
}

impl<'a> SynthMainThread<'a> {
    /// Wraps the shared state. The only field, and the only reason this type
    /// is no longer a unit struct.
    pub fn new(shared: &'a SynthShared) -> Self {
        Self { shared }
    }
}

impl<'a> PluginMainThread<'a, SynthShared> for SynthMainThread<'a> {}

fn apply_parameter_events(shared: &SynthShared, input: &InputEvents) {
    for event in input {
        if let Some(value) = event.as_event::<ParamValueEvent>() {
            if value.param_id() == Some(ClapId::new(0)) {
                shared.set_detune(value.value() as f32);
            }
        }
    }
}
impl PluginMainThreadParams for SynthMainThread<'_> {
    fn count(&mut self) -> u32 {
        1
    }
    fn get_info(&mut self, index: u32, writer: &mut ParamInfoWriter) {
        if index == 0 {
            writer.set(&ParamInfo {
                id: ClapId::new(0),
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Cookie::empty(),
                name: b"Detune",
                module: b"Pitch",
                min_value: -24.0,
                max_value: 24.0,
                default_value: 0.0,
            });
        }
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        (id == ClapId::new(0)).then(|| f64::from(self.shared.detune()))
    }
    fn value_to_text(
        &mut self,
        id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write;
        if id != ClapId::new(0) {
            return Err(core::fmt::Error);
        }
        write!(writer, "{value} st")
    }
    fn text_to_value(&mut self, id: ClapId, text: &std::ffi::CStr) -> Option<f64> {
        if id != ClapId::new(0) {
            return None;
        }
        text.to_str().ok()?.trim_end_matches(" st").parse().ok()
    }
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        apply_parameter_events(self.shared, input);
    }
}
impl PluginAudioProcessorParams for SynthProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        apply_parameter_events(self.shared, input);
    }
}

/// The CLAP `state` extension: eight bytes in, eight bytes out.
impl PluginStateImpl for SynthMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        output
            .write_all(&state_blob(self.shared.detune()))
            .map_err(|_| PluginError::Message("failed to write synth fixture state"))
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        // Bounded read: one byte more than the cap and the blob is refused
        // outright. `take` keeps a host that streams forever from growing the
        // buffer past `STATE_READ_CAP`.
        let mut bytes = [0_u8; STATE_READ_CAP];
        let mut cursor = 0_usize;
        loop {
            let read = input
                .read(&mut bytes[cursor..])
                .map_err(|_| PluginError::Message("failed to read synth fixture state"))?;
            if read == 0 {
                break;
            }
            cursor += read;
            if cursor == bytes.len() {
                // The buffer filled: either the blob is exactly the cap or it
                // is longer. Either way it is not ours, and refusing beats
                // parsing a prefix of something we do not understand.
                return Err(PluginError::Message("synth fixture state blob is too long"));
            }
        }

        // A blob that does not parse is an error, never a silent no-op: a host
        // that hands over the wrong bytes must find out here, not by wondering
        // later why the pitch never moved.
        let detune = parse_state(&bytes[..cursor])
            .ok_or(PluginError::Message("malformed synth fixture state blob"))?;
        self.shared.set_detune(detune);
        Ok(())
    }
}

impl PluginAudioPortsImpl for SynthMainThread<'_> {
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

impl PluginNotePortsImpl for SynthMainThread<'_> {
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
pub struct SynthProcessor<'a> {
    voices: [Voice; VOICES],
    /// The detune, read from here once per `process` call.
    ///
    /// A borrow rather than a copy because the value can change between blocks;
    /// a borrow that is only *loaded* once per block, rather than once per
    /// sample, because an atomic load in the inner sample loop would be a real
    /// cost for a value that cannot legitimately change mid-block.
    shared: &'a SynthShared,
    /// The detune snapshotted at the top of the current block, in semitones.
    /// Held so that a note-on landing mid-block uses the same value the rest
    /// of the block was tuned with.
    detune: f32,
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

impl SynthProcessor<'_> {
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
        voice.base_hz = key_to_hz(key);
        // Provisional, so a note that starts mid-block sounds at the right
        // pitch immediately; `retune` refreshes it at the top of every block.
        voice.phase_step = voice.base_hz * detune_ratio(self.detune) / self.sample_rate;
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

    /// Snapshot the shared detune and re-derive every active voice's phase
    /// step from its `base_hz`.
    ///
    /// Called once per block. That cadence is the audible contract of the state
    /// extension: a preset loaded while a key is held is heard on the very next
    /// block, without the note having to be re-triggered.
    fn retune(&mut self) {
        self.detune = self.shared.detune();
        let ratio = detune_ratio(self.detune);
        for voice in &mut self.voices {
            if voice.is_active() {
                voice.phase_step = voice.base_hz * ratio / self.sample_rate;
            }
        }
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

impl<'a> PluginAudioProcessor<'a, SynthShared, SynthMainThread<'a>> for SynthProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut SynthMainThread<'a>,
        shared: &'a SynthShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sample_rate = audio_config.sample_rate as f32;
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(PluginError::Message("invalid sample rate"));
        }
        Ok(Self {
            voices: [Voice::free(); VOICES],
            shared,
            detune: shared.detune(),
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
        apply_parameter_events(self.shared, events.input);
        // One atomic load per block, then every sounding voice is re-derived
        // from its base frequency. Allocation-free, and O(VOICES).
        self.retune();

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

impl SynthProcessor<'_> {
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

    #[test]
    fn the_state_blob_is_eight_bytes_of_magic_then_a_little_endian_float() {
        // A host test asserts on these bytes, so the layout is part of the
        // contract, not an implementation detail.
        let blob = state_blob(0.0);
        assert_eq!(blob.len(), 8);
        assert_eq!(&blob[..4], &STATE_MAGIC);
        assert_eq!(&blob[..4], b"MFS1");
        assert_eq!(&blob[4..], &0.0_f32.to_le_bytes());

        let blob = state_blob(12.0);
        assert_eq!(&blob[4..], &12.0_f32.to_le_bytes());
    }

    #[test]
    fn every_representable_detune_survives_a_blob_round_trip() {
        for detune in [
            0.0,
            12.0,
            -7.5,
            MAX_DETUNE_SEMITONES,
            -MAX_DETUNE_SEMITONES,
            0.5,
            -0.25,
        ] {
            let parsed = parse_state(&state_blob(detune));
            assert_eq!(parsed, Some(detune), "round trip of {detune}");
        }
    }

    #[test]
    fn parse_state_rejects_every_shape_of_bad_input_without_panicking() {
        // The host controls this byte slice entirely, so none of these may be
        // an index-out-of-bounds rather than a `None`.
        assert_eq!(parse_state(&[]), None, "empty");
        assert_eq!(parse_state(b"MFS"), None, "three bytes, a magic prefix");
        assert_eq!(parse_state(&[0; 4]), None, "magic-length but no payload");

        let mut wrong_magic = state_blob(3.0);
        wrong_magic[0] = b'X';
        assert_eq!(parse_state(&wrong_magic), None, "wrong magic");

        let mut too_long = state_blob(3.0);
        too_long.push(0);
        assert_eq!(parse_state(&too_long), None, "nine bytes");

        let mut truncated = state_blob(3.0);
        truncated.pop();
        assert_eq!(parse_state(&truncated), None, "seven bytes");
    }

    #[test]
    fn parse_state_rejects_a_blob_carrying_a_value_the_plugin_would_never_write() {
        // These blobs are well formed but the payload is not storable, so they
        // can only have come from a corrupt or foreign source.
        for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 25.0, -100.0] {
            let mut blob = STATE_MAGIC.to_vec();
            blob.extend_from_slice(&poison.to_le_bytes());
            assert_eq!(parse_state(&blob), None, "blob carrying {poison}");
        }
    }

    #[test]
    fn state_blob_clamps_out_of_range_and_zeroes_non_finite_input() {
        assert_eq!(parse_state(&state_blob(1000.0)), Some(MAX_DETUNE_SEMITONES));
        assert_eq!(
            parse_state(&state_blob(-1000.0)),
            Some(-MAX_DETUNE_SEMITONES)
        );
        for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(parse_state(&state_blob(poison)), Some(0.0), "{poison}");
        }
    }

    #[test]
    fn a_detune_of_one_octave_doubles_the_frequency() {
        assert_eq!(detune_ratio(0.0), 1.0, "no detune is exactly unity");
        assert!((detune_ratio(12.0) - 2.0).abs() < 1e-6);
        assert!((detune_ratio(-12.0) - 0.5).abs() < 1e-6);
        // The claim a host's FFT test rests on: key 69 detuned an octave up
        // sounds at 880 Hz.
        assert!((key_to_hz(69) * detune_ratio(12.0) - 880.0).abs() < 1e-2);
    }

    #[test]
    fn the_shared_detune_starts_at_zero_and_only_accepts_usable_values() {
        let shared = SynthShared::default();
        assert_eq!(shared.detune(), 0.0);

        shared.set_detune(-7.5);
        assert_eq!(shared.detune(), -7.5);

        // Out of range is clamped, not rejected: the value is still usable.
        shared.set_detune(99.0);
        assert_eq!(shared.detune(), MAX_DETUNE_SEMITONES);
        shared.set_detune(-99.0);
        assert_eq!(shared.detune(), -MAX_DETUNE_SEMITONES);

        // Non-finite is replaced by silence-safe zero rather than poisoning
        // every voice's phase step for the rest of the session.
        shared.set_detune(f32::NAN);
        assert_eq!(shared.detune(), 0.0);
        shared.set_detune(f32::INFINITY);
        assert_eq!(shared.detune(), 0.0);
    }
}
