//! The mixer: what a track strip does to a track's audio, and how the values it
//! does it with cross the real-time wall.
//!
//! ```text
//!   non-RT (UI command)                 audio thread (once per block)
//!   -------------------                 -----------------------------
//!   MixerParams::set_track_gain  --\
//!   MixerParams::set_track_pan   ---> atomics --> TrackStrip::mix_into
//!   MixerParams::set_track_muted --/              (per-sample linear ramp)
//!   MixerParams::set_master_gain ----> atomic --> MasterStrip::apply
//! ```
//!
//! # Why atomics and not a command queue
//! A fader is a *level*, not an event. Dropping a queued gain change would leave
//! the mixer at the wrong volume forever, and a queue deep enough never to drop
//! one is a queue sized for a mouse. An atomic `f32` has none of those problems:
//! the audio thread reads the newest value once per block, a write that lands
//! mid-block is simply heard on the next one, and there is nothing to overflow.
//! Transport commands stay on a queue because *they* are events, and their order
//! matters.
//!
//! # Why the ramp exists
//! Reading a new gain once per block and multiplying by it is a step change in
//! the waveform at the block boundary -- a click, and at 30 fader updates a
//! second, a zipper. So the strip never jumps: it walks from the coefficient it
//! left off at towards the new target at a bounded slope, taking
//! [`PARAM_RAMP_SECONDS`] to cross the whole distance. A block shorter than the
//! ramp gets part of the way and the next block continues, which is why the
//! current coefficient lives in the strip rather than being recomputed.
//!
//! Mute is the same mechanism with a target of zero: muting is a 10 ms fade, not
//! a cut, which is the difference between a mute button and a click.
//!
//! # The pan law
//! Equal power: the two channel gains are `cos θ` and `sin θ` for
//! `θ = (pan + 1)·π/4`, so `L² + R²` is constant across the whole sweep and a
//! source keeps its apparent loudness as it moves. That is the -3 dB law -- the
//! centre sits 3.01 dB below a hard-panned edge.
//!
//! **Normalisation.** The gains are scaled by `√2` so that *centre* is unity and
//! a hard-panned edge is +3.01 dB, rather than centre being -3.01 dB and the edge
//! unity. The law is the same law (the shape is what makes it equal-power, and
//! the 3.01 dB centre-to-edge relationship is asserted in the tests); only the
//! reference point differs. Anchoring at the centre is what lets a default
//! project -- every track at unity gain, centre pan -- sum to exactly what its
//! sources produced, so "a mixer was added" is not also "everything got 3 dB
//! quieter". See [`PAN_CENTRE_NORMALISATION`] to change it in one place.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::{AudioBuffers, CHANNELS};

/// Tracks the engine can run at once.
///
/// Fixed because the audio thread's per-track state is a fixed array: the meter
/// frame, the parameter block and the peak block are all `[_; MAX_TRACKS]`, so
/// nothing on the RT side has to grow. Sixteen matches
/// [`MAX_ROWS`](../../mydaw_plugin_sequencer/pattern/constant.MAX_ROWS.html) in
/// the sequencer and the 16 channels a [`NoteEvent`](crate::NoteEvent) can
/// address, which is not a coincidence: a note channel *is* a track index.
pub const MAX_TRACKS: usize = 16;

/// Buses the engine can run at once.
///
/// Fixed for exactly the reason [`MAX_TRACKS`] is: every piece of per-bus state
/// the audio thread touches -- the parameter block, the send matrix, the peak
/// block, the meter frame -- is a `[_; MAX_BUSES]`, so nothing on the RT side
/// ever grows. Eight is the number of aux/group buses a mixer of sixteen
/// channels needs before the *routing* rather than the count becomes the
/// limitation, and it keeps the send matrix (see [`MixerParams`]) at
/// `(16 + 8) x 8` = 192 entries, which is under two kilobytes.
pub const MAX_BUSES: usize = 8;

/// Strips a send can originate from: every track plus every bus.
///
/// The send matrix is indexed by [`StripId::slot`], so this is its first
/// dimension.
pub const MAX_SEND_SOURCES: usize = MAX_TRACKS + MAX_BUSES;

/// Loudest a send tap may be, as a linear multiplier.
///
/// `1.0` is unity -- the tap carries exactly what the strip produced -- and the
/// headroom above it is the +1.94 dB a send knob is conventionally allowed to
/// push, so that a reverb can be driven harder than its source without touching
/// the source. Anything above this is a mistake rather than a gesture, and is
/// clamped.
pub const MAX_SEND_LEVEL: f32 = 1.25;

/// [`MAX_SEND_LEVEL`] in decibels: `+1.9382`.
pub fn max_send_db() -> f32 {
    gain_to_db(MAX_SEND_LEVEL)
}

/// How long a gain, pan or mute change takes to complete, in seconds.
///
/// 10 ms is the standard mixer fade: below the ~20 ms at which a level change
/// starts to be heard as a gesture rather than an edit, and far above the
/// ~0.1 ms at which a step becomes an audible click.
pub const PARAM_RAMP_SECONDS: f64 = 0.010;

/// Shortest ramp, in frames, whatever the sample rate says.
///
/// At 8 kHz a 10 ms ramp is 80 frames; this floor only matters for the
/// artificially low rates tests use, where a two-frame ramp would be a step in
/// all but name.
pub const PARAM_RAMP_MIN_FRAMES: usize = 64;

/// Scale applied to both pan gains so that centre pan is unity. See the module
/// docs for why this is not 1.0.
pub const PAN_CENTRE_NORMALISATION: f32 = std::f32::consts::SQRT_2;

/// Ramp length in frames at `sample_rate`.
///
/// Always at least [`PARAM_RAMP_MIN_FRAMES`], and always at least 1 so the
/// division in [`TrackStrip::mix_into`] can never produce an infinity.
pub fn param_ramp_frames(sample_rate: f64) -> usize {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return PARAM_RAMP_MIN_FRAMES;
    }
    ((sample_rate * PARAM_RAMP_SECONDS).round() as usize).max(PARAM_RAMP_MIN_FRAMES)
}

/// The two channel gains for a pan position in `-1.0..=1.0`.
///
/// `-1` is hard left, `0` centre, `+1` hard right. Equal power, normalised so
/// centre is unity -- see the module docs.
///
/// Computed in `f64` and rounded once: at centre the naive `f32` product lands a
/// bit either side of 1.0, and a "unity" gain that is actually 0.99999994 is the
/// kind of thing that makes a round-trip test fail for no musical reason.
pub fn pan_gains(pan: f32) -> [f32; CHANNELS] {
    let pan = if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let theta = (f64::from(pan) + 1.0) * std::f64::consts::FRAC_PI_4;
    let scale = f64::from(PAN_CENTRE_NORMALISATION);
    [(theta.cos() * scale) as f32, (theta.sin() * scale) as f32]
}

/// Convert a gain in decibels to a linear multiplier.
///
/// `NEG_INFINITY` (and anything at or below [`SILENCE_DB`]) is exactly zero, so
/// a fader pulled to the bottom is silence rather than -60 dB of hiss.
pub fn db_to_gain(db: f32) -> f32 {
    if !db.is_finite() || db <= SILENCE_DB {
        return 0.0;
    }
    10.0_f32.powf(db / 20.0)
}

/// Convert a linear multiplier to decibels, flooring at [`SILENCE_DB`].
pub fn gain_to_db(gain: f32) -> f32 {
    if !gain.is_finite() || gain <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * gain.log10()).max(SILENCE_DB)
}

/// Bottom of the fader scale, in dB. A fader here means silence.
pub const SILENCE_DB: f32 = -60.0;

/// Top of the fader scale, in dB.
pub const MAX_GAIN_DB: f32 = 6.0;

// ---------------------------------------------------------------- parameters

/// Which strip a send comes from.
///
/// Tracks and buses are separate index spaces everywhere a user can see them --
/// "track 3" and "bus 3" are different strips -- so a send's source has to say
/// *which* three it means. [`slot`](Self::slot) flattens the pair into the one
/// number the send matrix is indexed by, and nothing outside this module needs
/// to know what that number is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StripId {
    /// A track, by index. Also its note channel -- see [`crate::graph`].
    Track(usize),
    /// A bus, by index.
    Bus(usize),
}

impl StripId {
    /// Row of the send matrix this strip owns, or `None` past the fixed limits.
    ///
    /// Tracks occupy `0..MAX_TRACKS` and buses the [`MAX_BUSES`] rows after
    /// them, so the two never collide and neither has to be renumbered when the
    /// other grows.
    pub const fn slot(self) -> Option<usize> {
        match self {
            Self::Track(index) if index < MAX_TRACKS => Some(index),
            Self::Bus(index) if index < MAX_BUSES => Some(MAX_TRACKS + index),
            _ => None,
        }
    }

    /// Whether this names a bus.
    pub const fn is_bus(self) -> bool {
        matches!(self, Self::Bus(_))
    }

    /// The index, whichever kind this is.
    pub const fn index(self) -> usize {
        match self {
            Self::Track(index) | Self::Bus(index) => index,
        }
    }
}

impl std::fmt::Display for StripId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Track(index) => write!(f, "track {index}"),
            Self::Bus(index) => write!(f, "bus {index}"),
        }
    }
}

/// One send tap's live values, as the audio thread reads them.
///
/// Both are *levels*, not events, so both are atomics for the reason the whole
/// of [`MixerParams`] is: a dropped queue entry would leave a send at the wrong
/// level for ever. **Changing either one costs no graph rebuild** -- the tap is
/// already in the compiled graph, and this is all it reads.
///
/// Whether a tap *exists* is structural and does live in the graph; see
/// [`GraphSpec::push_send`](crate::GraphSpec::push_send).
#[derive(Debug)]
pub struct SendParams {
    /// Linear level, `0.0..=`[`MAX_SEND_LEVEL`], as `f32::to_bits`.
    level: AtomicU32,
    /// Whether the tap is taken **before** the source strip's fader, pan and
    /// mute. See [`crate::graph`] for exactly where "before" is.
    pre: AtomicBool,
}

impl Default for SendParams {
    /// Unity, post-fader -- the setting a freshly created send should have, and
    /// the one a matrix cell nobody has touched must not be able to make audible
    /// on its own (no tap exists until the graph carries one).
    fn default() -> Self {
        Self {
            level: AtomicU32::new(1.0_f32.to_bits()),
            pre: AtomicBool::new(false),
        }
    }
}

/// A send level the graph can safely multiply by.
fn sane_send_level(level: f32) -> f32 {
    if level.is_finite() && level >= 0.0 {
        level.min(MAX_SEND_LEVEL)
    } else {
        0.0
    }
}

/// One track's mixer parameters, as the audio thread reads them.
///
/// `muted` is the **effective** mute: solo is resolved on the non-RT side and
/// folded in here, so the audio thread never has to know that solo exists. That
/// is deliberate -- "is any track soloed" is a question about the whole project,
/// and answering it per block per track would be the RT thread doing the UI's
/// arithmetic.
#[derive(Debug)]
pub struct TrackParams {
    /// Linear gain, as `f32::to_bits`.
    gain: AtomicU32,
    /// Pan in `-1.0..=1.0`, as `f32::to_bits`.
    pan: AtomicU32,
    /// Effective mute: the track's own mute, or "something else is soloed".
    muted: AtomicBool,
}

impl Default for TrackParams {
    /// Unity gain, centred, unmuted.
    fn default() -> Self {
        Self {
            gain: AtomicU32::new(1.0_f32.to_bits()),
            pan: AtomicU32::new(0.0_f32.to_bits()),
            muted: AtomicBool::new(false),
        }
    }
}

fn load_f32(cell: &AtomicU32) -> f32 {
    f32::from_bits(cell.load(Ordering::Relaxed))
}

fn store_f32(cell: &AtomicU32, value: f32) {
    cell.store(value.to_bits(), Ordering::Relaxed);
}

/// Every track's parameters plus the master fader, shared between the UI and the
/// audio thread.
///
/// Held by the compiled graph as an `Arc`, so a graph rebuild (adding a track,
/// say) hands the *same* parameter block to the new graph and the mixer keeps
/// its settings across the swap.
#[derive(Debug)]
pub struct MixerParams {
    tracks: [TrackParams; MAX_TRACKS],
    /// Every bus's strip. Identical in shape to a track's, because a bus *is* a
    /// strip: it sums, it has a fader, a pan and a mute, and it has its own
    /// insert chain. What it does not have is a source.
    buses: [TrackParams; MAX_BUSES],
    /// The send matrix, `[source slot][destination bus]`.
    ///
    /// A fixed matrix rather than a list, so that a send's level and pre/post
    /// flag can be written from a command thread and read from the audio thread
    /// **without either of them agreeing on an index first**. The graph carries
    /// the taps that exist and looks each one's cell up by the pair it already
    /// knows; a cell no tap points at is simply never read.
    sends: [[SendParams; MAX_BUSES]; MAX_SEND_SOURCES],
    /// Linear master gain, as `f32::to_bits`.
    master_gain: AtomicU32,
}

impl Default for MixerParams {
    fn default() -> Self {
        Self {
            tracks: std::array::from_fn(|_| TrackParams::default()),
            buses: std::array::from_fn(|_| TrackParams::default()),
            sends: std::array::from_fn(|_| std::array::from_fn(|_| SendParams::default())),
            master_gain: AtomicU32::new(1.0_f32.to_bits()),
        }
    }
}

impl MixerParams {
    /// A parameter block with every track at unity gain, centred and unmuted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Linear gain of `track`, or `0.0` for an index past [`MAX_TRACKS`].
    pub fn track_gain(&self, track: usize) -> f32 {
        self.tracks.get(track).map_or(0.0, |t| load_f32(&t.gain))
    }

    /// Set the linear gain of `track`. Out-of-range indices are ignored.
    ///
    /// Non-finite and negative values are refused rather than stored: a NaN in
    /// the strip would turn the whole mix into NaN one block later, and there is
    /// no user gesture that means "multiply by minus one".
    pub fn set_track_gain(&self, track: usize, gain: f32) {
        if let Some(params) = self.tracks.get(track) {
            store_f32(&params.gain, sane_gain(gain));
        }
    }

    /// Pan of `track` in `-1.0..=1.0`, or `0.0` for an index past
    /// [`MAX_TRACKS`].
    pub fn track_pan(&self, track: usize) -> f32 {
        self.tracks.get(track).map_or(0.0, |t| load_f32(&t.pan))
    }

    /// Set the pan of `track`, clamped to `-1.0..=1.0`.
    pub fn set_track_pan(&self, track: usize, pan: f32) {
        if let Some(params) = self.tracks.get(track) {
            let pan = if pan.is_finite() {
                pan.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            store_f32(&params.pan, pan);
        }
    }

    /// Whether `track` is effectively muted (its own mute, or another track's
    /// solo).
    pub fn track_muted(&self, track: usize) -> bool {
        self.tracks
            .get(track)
            .is_some_and(|t| t.muted.load(Ordering::Relaxed))
    }

    /// Set the effective mute of `track`. Solo is resolved by the caller.
    pub fn set_track_muted(&self, track: usize, muted: bool) {
        if let Some(params) = self.tracks.get(track) {
            params.muted.store(muted, Ordering::Relaxed);
        }
    }

    /// Linear master gain.
    pub fn master_gain(&self) -> f32 {
        load_f32(&self.master_gain)
    }

    /// Set the linear master gain.
    pub fn set_master_gain(&self, gain: f32) {
        store_f32(&self.master_gain, sane_gain(gain));
    }

    /// The channel coefficients `track` should be heading towards this block.
    ///
    /// One read of three atomics plus the pan law -- called once per track per
    /// block, never per sample.
    pub fn track_target(&self, track: usize) -> [f32; CHANNELS] {
        let Some(params) = self.tracks.get(track) else {
            return [0.0; CHANNELS];
        };
        if params.muted.load(Ordering::Relaxed) {
            return [0.0; CHANNELS];
        }
        let gain = load_f32(&params.gain);
        let pan = pan_gains(load_f32(&params.pan));
        [gain * pan[0], gain * pan[1]]
    }

    // ----------------------------------------------------------------- buses

    /// Linear gain of `bus`, or `0.0` past [`MAX_BUSES`].
    pub fn bus_gain(&self, bus: usize) -> f32 {
        self.buses.get(bus).map_or(0.0, |b| load_f32(&b.gain))
    }

    /// Set the linear gain of `bus`. Out-of-range indices are ignored.
    pub fn set_bus_gain(&self, bus: usize, gain: f32) {
        if let Some(params) = self.buses.get(bus) {
            store_f32(&params.gain, sane_gain(gain));
        }
    }

    /// Pan of `bus`, or `0.0` past [`MAX_BUSES`].
    pub fn bus_pan(&self, bus: usize) -> f32 {
        self.buses.get(bus).map_or(0.0, |b| load_f32(&b.pan))
    }

    /// Set the pan of `bus`, clamped to `-1.0..=1.0`.
    pub fn set_bus_pan(&self, bus: usize, pan: f32) {
        if let Some(params) = self.buses.get(bus) {
            let pan = if pan.is_finite() {
                pan.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            store_f32(&params.pan, pan);
        }
    }

    /// Whether `bus` is effectively muted.
    pub fn bus_muted(&self, bus: usize) -> bool {
        self.buses
            .get(bus)
            .is_some_and(|b| b.muted.load(Ordering::Relaxed))
    }

    /// Set the effective mute of `bus`. Solo is resolved by the caller, exactly
    /// as it is for a track.
    pub fn set_bus_muted(&self, bus: usize, muted: bool) {
        if let Some(params) = self.buses.get(bus) {
            params.muted.store(muted, Ordering::Relaxed);
        }
    }

    /// The channel coefficients `bus` should be heading towards this block.
    pub fn bus_target(&self, bus: usize) -> [f32; CHANNELS] {
        let Some(params) = self.buses.get(bus) else {
            return [0.0; CHANNELS];
        };
        if params.muted.load(Ordering::Relaxed) {
            return [0.0; CHANNELS];
        }
        let gain = load_f32(&params.gain);
        let pan = pan_gains(load_f32(&params.pan));
        [gain * pan[0], gain * pan[1]]
    }

    // ----------------------------------------------------------------- sends

    /// The matrix cell for a `(source, destination bus)` pair.
    fn send_cell(&self, source: StripId, dest: usize) -> Option<&SendParams> {
        self.sends.get(source.slot()?)?.get(dest)
    }

    /// Linear level of the tap from `source` into bus `dest`.
    ///
    /// `0.0` for a pair outside the fixed limits, which is also what a tap that
    /// does not exist would contribute.
    pub fn send_level(&self, source: StripId, dest: usize) -> f32 {
        self.send_cell(source, dest)
            .map_or(0.0, |cell| load_f32(&cell.level))
    }

    /// Set a tap's level, clamped to `0.0..=`[`MAX_SEND_LEVEL`].
    ///
    /// **No graph rebuild.** The audio thread reads this once per block and
    /// ramps towards it, so a send knob drag costs exactly what a fader drag
    /// costs.
    pub fn set_send_level(&self, source: StripId, dest: usize, level: f32) {
        if let Some(cell) = self.send_cell(source, dest) {
            store_f32(&cell.level, sane_send_level(level));
        }
    }

    /// Whether the tap from `source` into bus `dest` is taken before the
    /// source's fader.
    pub fn send_pre(&self, source: StripId, dest: usize) -> bool {
        self.send_cell(source, dest)
            .is_some_and(|cell| cell.pre.load(Ordering::Relaxed))
    }

    /// Move a tap between pre- and post-fader. **No graph rebuild.**
    ///
    /// The tap itself is in the graph twice over -- once reading the pre-fader
    /// buffer and once the post-fader one -- and this atomic says which of the
    /// two is live. See [`crate::graph`].
    pub fn set_send_pre(&self, source: StripId, dest: usize, pre: bool) {
        if let Some(cell) = self.send_cell(source, dest) {
            cell.pre.store(pre, Ordering::Relaxed);
        }
    }

    /// The pre- and post-fader coefficients a tap should be heading towards.
    ///
    /// Exactly one of the two is the level and the other is zero, so the flag
    /// is a **crossfade between two taps** rather than a branch that reroutes
    /// audio on a block boundary: flipping pre/post mid-play ramps one tap down
    /// while the other ramps up, and neither steps.
    pub fn send_targets(&self, source: StripId, dest: usize) -> (f32, f32) {
        let Some(cell) = self.send_cell(source, dest) else {
            return (0.0, 0.0);
        };
        let level = load_f32(&cell.level);
        if cell.pre.load(Ordering::Relaxed) {
            (level, 0.0)
        } else {
            (0.0, level)
        }
    }
}

/// A gain the strip can safely multiply by: finite and non-negative.
fn sane_gain(gain: f32) -> f32 {
    if gain.is_finite() && gain >= 0.0 {
        gain
    } else {
        0.0
    }
}

// -------------------------------------------------------------------- strips

/// Per-track smoothed gain, and the sum into the master bus.
///
/// Owns two coefficients (one per channel) and walks them towards the target the
/// parameter block asks for. Everything else about a track -- its source, its
/// inserts, its scratch buffer -- belongs to the graph.
#[derive(Debug)]
pub struct TrackStrip {
    current: [f32; CHANNELS],
    /// Where each coefficient is heading. Compared against the block's target to
    /// notice that somebody moved a control.
    goal: [f32; CHANNELS],
    /// Frames left in the ramp, per channel. Zero means "settled".
    remaining: [u32; CHANNELS],
    /// Frames a full-distance ramp takes. Never zero.
    ramp_frames: u32,
}

impl TrackStrip {
    /// A strip already sitting at `target`.
    ///
    /// Starting *at* the target rather than at zero is what makes a graph
    /// rebuild inaudible: a new graph picks up the mixer where the old one left
    /// it instead of fading in over 10 ms every time a track is added.
    pub fn new(sample_rate: f64, target: [f32; CHANNELS]) -> Self {
        Self {
            current: target,
            goal: target,
            remaining: [0; CHANNELS],
            ramp_frames: param_ramp_frames(sample_rate) as u32,
        }
    }

    /// The coefficients in force at the last sample processed.
    pub fn current(&self) -> [f32; CHANNELS] {
        self.current
    }

    /// Frames a full-distance ramp takes at this strip's sample rate.
    pub fn ramp_frames(&self) -> usize {
        self.ramp_frames as usize
    }

    /// Whether both coefficients have reached their target.
    pub fn is_settled(&self) -> bool {
        self.remaining == [0; CHANNELS]
    }

    /// Scale `src` towards `target` and **add** it into `dst`, returning the
    /// per-channel peak of what was added.
    ///
    /// The peak is measured after the strip, which is what a channel meter
    /// shows: a muted track reads zero, and a track pulled down reads the level
    /// it is contributing rather than the level it is producing.
    ///
    /// # RT safety
    /// Arithmetic over pre-allocated slices. No allocation, no branches that can
    /// loop unboundedly, and the frame count is the shorter of the two buffers'
    /// valid regions so a mismatched block size truncates instead of reading
    /// past the end.
    pub fn mix_into(
        &mut self,
        src: &AudioBuffers,
        dst: &mut AudioBuffers,
        target: [f32; CHANNELS],
    ) -> [f32; CHANNELS] {
        self.run::<true>(src, dst, target)
    }

    /// Scale `src` towards `target` and **write** it over `dst`, returning the
    /// per-channel peak of what was written.
    ///
    /// The same arithmetic as [`mix_into`](Self::mix_into) with an assignment
    /// where that has an accumulation, so the two are bit-identical against a
    /// silent destination. It exists because a **post-fader send** needs the
    /// post-strip signal as a *value*: the master sum consumes it, but so does
    /// every bus the strip taps into, and computing it once into a scratch
    /// buffer is the only way to hand the same samples to more than one
    /// destination.
    ///
    /// # RT safety
    /// As [`mix_into`](Self::mix_into).
    pub fn apply_into(
        &mut self,
        src: &AudioBuffers,
        dst: &mut AudioBuffers,
        target: [f32; CHANNELS],
    ) -> [f32; CHANNELS] {
        self.run::<false>(src, dst, target)
    }

    /// `ADD` chooses accumulation or assignment. A const parameter rather than a
    /// `bool` argument so the branch is gone before the loop is.
    fn run<const ADD: bool>(
        &mut self,
        src: &AudioBuffers,
        dst: &mut AudioBuffers,
        target: [f32; CHANNELS],
    ) -> [f32; CHANNELS] {
        let source = src.as_slices();
        let mut destination = dst.as_mut_slices();
        let mut peak = [0.0_f32; CHANNELS];

        for channel in 0..CHANNELS {
            let input = source[channel];
            let output = &mut destination[channel];
            let frames = input.len().min(output.len());

            // A control moved: restart the ramp from wherever this coefficient
            // happens to be, which is what makes a change during a ramp
            // continuous rather than a jump back to the old start.
            if target[channel] != self.goal[channel] {
                self.goal[channel] = target[channel];
                self.remaining[channel] = if self.current[channel] == target[channel] {
                    0
                } else {
                    self.ramp_frames
                };
            }

            let goal = self.goal[channel];
            let mut gain = self.current[channel];
            let mut remaining = self.remaining[channel];

            if remaining == 0 {
                // Steady state, which is every block in which nobody touched a
                // control: one multiply-add per sample and no branch.
                for frame in 0..frames {
                    let value = input[frame] * gain;
                    peak[channel] = peak[channel].max(value.abs());
                    if ADD {
                        output[frame] += value;
                    } else {
                        output[frame] = value;
                    }
                }
            } else {
                // The step is derived from the frames *left*, so the ramp always
                // takes exactly `ramp_frames` from the moment the control moved
                // and lands on the target exactly rather than near it.
                let step = (goal - gain) / remaining as f32;
                for frame in 0..frames {
                    if remaining > 0 {
                        remaining -= 1;
                        gain = if remaining == 0 { goal } else { gain + step };
                    }
                    let value = input[frame] * gain;
                    peak[channel] = peak[channel].max(value.abs());
                    if ADD {
                        output[frame] += value;
                    } else {
                        output[frame] = value;
                    }
                }
                self.current[channel] = gain;
                self.remaining[channel] = remaining;
            }
        }
        peak
    }
}

/// The master fader: one smoothed gain applied in place to the summed bus.
///
/// No pan -- the master bus is already stereo, and a "pan" on it would be a
/// balance control, which is a different thing and not one this step needs.
#[derive(Debug)]
pub struct MasterStrip {
    current: f32,
    goal: f32,
    remaining: u32,
    ramp_frames: u32,
}

impl MasterStrip {
    /// A master strip already sitting at `target`.
    pub fn new(sample_rate: f64, target: f32) -> Self {
        Self {
            current: target,
            goal: target,
            remaining: 0,
            ramp_frames: param_ramp_frames(sample_rate) as u32,
        }
    }

    /// The gain in force at the last sample processed.
    pub fn current(&self) -> f32 {
        self.current
    }

    /// Frames a full-distance ramp takes at this strip's sample rate.
    pub fn ramp_frames(&self) -> usize {
        self.ramp_frames as usize
    }

    /// Apply the fader to `buffers` in place, ramping towards `target`.
    ///
    /// Both channels share one coefficient and one ramp -- they are stepped in
    /// lockstep, so the master fader can never smear the stereo image while it
    /// moves.
    pub fn apply(&mut self, buffers: &mut AudioBuffers, target: f32) {
        if target != self.goal {
            self.goal = target;
            self.remaining = if self.current == target {
                0
            } else {
                self.ramp_frames
            };
        }

        if self.remaining == 0 {
            if self.current == 1.0 {
                return; // Unity: the overwhelmingly common case costs nothing.
            }
            let gain = self.current;
            for channel in buffers.as_mut_slices() {
                for sample in channel.iter_mut() {
                    *sample *= gain;
                }
            }
            return;
        }

        let goal = self.goal;
        let step = (goal - self.current) / self.remaining as f32;
        let mut gain = self.current;
        let mut remaining = self.remaining;
        let mut channels = buffers.as_mut_slices();
        let frames = channels[0].len().min(channels[1].len());

        for frame in 0..frames {
            if remaining > 0 {
                remaining -= 1;
                gain = if remaining == 0 { goal } else { gain + step };
            }
            for channel in channels.iter_mut() {
                channel[frame] *= gain;
            }
        }
        self.current = gain;
        self.remaining = remaining;
    }
}

// -------------------------------------------------------------------- meters

/// One track's post-strip peak for a single block.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TrackPeak {
    pub peak_l: f32,
    pub peak_r: f32,
}

impl TrackPeak {
    /// A silent block.
    pub const SILENT: Self = Self {
        peak_l: 0.0,
        peak_r: 0.0,
    };

    /// The louder of the two channels.
    pub fn max(&self) -> f32 {
        self.peak_l.max(self.peak_r)
    }
}

/// Every strip's peak for one block, in a fixed array the audio thread owns.
///
/// `Copy`, so the meter path reads it out of the graph by value with no lock and
/// no queue of its own. `N` is the fixed strip count of whatever it is
/// measuring -- [`TrackPeaks`] for the tracks, [`BusPeaks`] for the buses. One
/// type rather than two identical ones, because the two differ in exactly one
/// number and a copy of this that drifted would be a meter that lies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeakBlock<const N: usize> {
    peaks: [TrackPeak; N],
    count: usize,
}

impl<const N: usize> Default for PeakBlock<N> {
    fn default() -> Self {
        Self::SILENT
    }
}

impl<const N: usize> PeakBlock<N> {
    /// No strips, no level.
    pub const SILENT: Self = Self {
        peaks: [TrackPeak::SILENT; N],
        count: 0,
    };

    /// A block of `count` silent strips.
    pub fn empty(count: usize) -> Self {
        Self {
            peaks: [TrackPeak::SILENT; N],
            count: count.min(N),
        }
    }

    /// Strips the graph that produced this block was running.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Peak of `strip`, or [`TrackPeak::SILENT`] past the end.
    pub fn get(&self, strip: usize) -> TrackPeak {
        if strip < self.count {
            self.peaks[strip]
        } else {
            TrackPeak::SILENT
        }
    }

    /// Record `strip`'s peak. Indices past the fixed limit are ignored.
    pub fn set(&mut self, strip: usize, peak: TrackPeak) {
        if strip < N {
            self.peaks[strip] = peak;
            if strip >= self.count {
                self.count = strip + 1;
            }
        }
    }

    /// The peaks, as a slice of exactly [`count`](Self::count) entries.
    pub fn as_slice(&self) -> &[TrackPeak] {
        &self.peaks[..self.count]
    }
}

/// Every track's post-strip peak for one block.
pub type TrackPeaks = PeakBlock<MAX_TRACKS>;

/// Every bus's post-strip peak for one block.
pub type BusPeaks = PeakBlock<MAX_BUSES>;

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn buffers_filled(frames: usize, value: f32) -> AudioBuffers {
        let mut b = AudioBuffers::new(frames);
        for channel in b.as_mut_slices() {
            channel.fill(value);
        }
        b
    }

    fn silent(frames: usize) -> AudioBuffers {
        AudioBuffers::new(frames)
    }

    fn db(linear: f32) -> f32 {
        20.0 * linear.log10()
    }

    #[test]
    fn the_pan_law_is_equal_power_with_the_centre_at_unity() {
        let centre = pan_gains(0.0);
        assert!((centre[0] - 1.0).abs() < 1e-6, "{centre:?}");
        assert!((centre[1] - 1.0).abs() < 1e-6, "{centre:?}");

        let left = pan_gains(-1.0);
        assert!((left[0] - std::f32::consts::SQRT_2).abs() < 1e-6);
        assert_eq!(left[1], 0.0, "hard left must put nothing in the right");

        let right = pan_gains(1.0);
        assert!((right[1] - std::f32::consts::SQRT_2).abs() < 1e-6);
        assert!(
            db(right[0].max(f32::MIN_POSITIVE)) < -80.0,
            "hard right leaves {} in the left channel",
            right[0]
        );

        // The defining property: constant power across the whole sweep.
        for step in 0..=20 {
            let pan = -1.0 + step as f32 / 10.0;
            let [l, r] = pan_gains(pan);
            let power = l * l + r * r;
            assert!(
                (power - 2.0).abs() < 1e-5,
                "pan {pan}: power {power} is not constant"
            );
        }

        // ...and therefore the -3.01 dB centre the law is named after, measured
        // from centre to a hard-panned edge.
        let centre_to_edge = db(centre[0]) - db(left[0]);
        assert!(
            (centre_to_edge + 3.0103).abs() < 0.01,
            "centre should sit 3.01 dB below the edge, got {centre_to_edge}"
        );
    }

    #[test]
    fn a_pan_value_the_ui_should_never_have_sent_is_clamped_not_propagated() {
        assert_eq!(pan_gains(-5.0), pan_gains(-1.0));
        assert_eq!(pan_gains(5.0), pan_gains(1.0));
        assert_eq!(pan_gains(f32::NAN), pan_gains(0.0));
    }

    #[test]
    fn decibels_round_trip_and_the_bottom_of_the_scale_is_silence() {
        assert!((db_to_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert_eq!(db_to_gain(SILENCE_DB), 0.0, "the fader floor is silence");
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert_eq!(db_to_gain(f32::NAN), 0.0);

        assert!((gain_to_db(1.0)).abs() < 1e-6);
        assert_eq!(gain_to_db(0.0), SILENCE_DB);
        for db_value in [-48.0_f32, -12.0, 0.0, 6.0] {
            let round_tripped = gain_to_db(db_to_gain(db_value));
            assert!((round_tripped - db_value).abs() < 1e-3, "{db_value}");
        }
    }

    #[test]
    fn parameters_default_to_unity_centre_unmuted_and_reject_nonsense() {
        let params = MixerParams::new();
        for track in 0..MAX_TRACKS {
            assert_eq!(params.track_gain(track), 1.0);
            assert_eq!(params.track_pan(track), 0.0);
            assert!(!params.track_muted(track));
            let target = params.track_target(track);
            assert!((target[0] - 1.0).abs() < 1e-6 && (target[1] - 1.0).abs() < 1e-6);
        }
        assert_eq!(params.master_gain(), 1.0);

        params.set_track_gain(0, f32::NAN);
        assert_eq!(params.track_gain(0), 0.0, "a NaN gain would poison the mix");
        params.set_track_gain(0, -2.0);
        assert_eq!(params.track_gain(0), 0.0);
        params.set_track_pan(0, 9.0);
        assert_eq!(params.track_pan(0), 1.0, "clamped, not stored");

        // Out-of-range indices are inert rather than panicking: the index comes
        // from a webview.
        params.set_track_gain(MAX_TRACKS, 0.5);
        assert_eq!(params.track_gain(MAX_TRACKS), 0.0);
        assert_eq!(params.track_target(MAX_TRACKS), [0.0, 0.0]);
    }

    #[test]
    fn a_muted_track_targets_silence_whatever_its_fader_says() {
        let params = MixerParams::new();
        params.set_track_gain(2, 0.9);
        params.set_track_muted(2, true);
        assert_eq!(params.track_target(2), [0.0, 0.0]);
        params.set_track_muted(2, false);
        let target = params.track_target(2);
        assert!((target[0] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn a_strip_at_its_target_is_a_plain_multiply_and_add() {
        let mut strip = TrackStrip::new(SR, [1.0, 1.0]);
        let src = buffers_filled(32, 0.5);
        let mut dst = buffers_filled(32, 0.25);
        let peak = strip.mix_into(&src, &mut dst, [1.0, 1.0]);

        assert_eq!(peak, [0.5, 0.5]);
        assert!(dst.as_slices()[0].iter().all(|s| (*s - 0.75).abs() < 1e-6));
        assert_eq!(strip.current(), [1.0, 1.0]);
    }

    #[test]
    fn a_strip_pans_hard_left_into_the_left_channel_only() {
        let target = pan_gains(-1.0);
        let mut strip = TrackStrip::new(SR, target);
        let src = buffers_filled(64, 1.0);
        let mut dst = silent(64);
        strip.mix_into(&src, &mut dst, target);

        let [left, right] = dst.as_slices();
        assert!(left
            .iter()
            .all(|s| (*s - std::f32::consts::SQRT_2).abs() < 1e-6));
        assert!(
            right.iter().all(|s| *s == 0.0),
            "hard left must leave the right channel untouched"
        );
    }

    /// The zipper-noise test. A constant input means the output *is* the gain
    /// coefficient, so the sample-to-sample difference is exactly the ramp step.
    #[test]
    fn a_gain_change_ramps_instead_of_stepping() {
        let mut strip = TrackStrip::new(SR, [1.0, 1.0]);
        let ramp = strip.ramp_frames();
        assert_eq!(ramp, 480, "10 ms at 48 kHz");

        let src = buffers_filled(2048, 1.0);
        let mut dst = silent(2048);
        // Straight to silence: the largest jump a mute could ever ask for.
        strip.mix_into(&src, &mut dst, [0.0, 0.0]);

        let left = dst.as_slices()[0];
        let limit = 1.0 / ramp as f32 + 1e-7;
        let worst = left
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            worst <= limit,
            "step {worst} exceeds the ramp limit {limit}"
        );

        assert_eq!(left[0], 1.0 - 1.0 / ramp as f32, "the first sample moves");
        assert_eq!(left[ramp - 1], 0.0, "the ramp completes in one ramp length");
        assert!(left[ramp..].iter().all(|s| *s == 0.0));
        assert_eq!(strip.current(), [0.0, 0.0]);
    }

    #[test]
    fn a_ramp_longer_than_the_block_continues_on_the_next_one() {
        let mut strip = TrackStrip::new(SR, [1.0, 1.0]);
        let ramp = strip.ramp_frames();
        let src = buffers_filled(64, 1.0);

        let mut first = silent(64);
        strip.mix_into(&src, &mut first, [0.0, 0.0]);
        let after_one_block = strip.current()[0];
        assert!(
            (after_one_block - (1.0 - 64.0 / ramp as f32)).abs() < 1e-5,
            "one 64-frame block of a {ramp}-frame ramp, got {after_one_block}"
        );

        // Carrying on from where it left off is the whole point of the strip
        // owning its coefficient.
        let mut second = silent(64);
        strip.mix_into(&src, &mut second, [0.0, 0.0]);
        let boundary = (second.as_slices()[0][0] - first.as_slices()[0][63]).abs();
        assert!(
            boundary <= 1.0 / ramp as f32 + 1e-7,
            "the block boundary is a step of {boundary}"
        );
    }

    #[test]
    fn the_two_channels_of_a_strip_finish_their_ramps_together() {
        // Different distances, same duration -- otherwise a pan change would
        // smear the image while it moved.
        let mut strip = TrackStrip::new(SR, [1.0, 1.0]);
        let ramp = strip.ramp_frames();
        let src = buffers_filled(ramp, 1.0);
        let mut dst = silent(ramp);
        strip.mix_into(&src, &mut dst, [0.25, 0.0]);
        assert_eq!(strip.current(), [0.25, 0.0]);
    }

    #[test]
    fn the_master_strip_applies_one_ramp_to_both_channels() {
        let mut master = MasterStrip::new(SR, 1.0);
        let mut buffers = buffers_filled(1024, 1.0);
        master.apply(&mut buffers, 0.5);

        let [left, right] = buffers.as_slices();
        assert_eq!(left, right, "one coefficient, so the channels must agree");
        assert_eq!(left[0], 1.0 - 0.5 / master.ramp_frames() as f32);
        assert!((left[1023] - 0.5).abs() < 1e-7);
        assert!((master.current() - 0.5).abs() < 1e-7);

        // Settled: a second block is a flat multiply.
        let mut next = buffers_filled(16, 1.0);
        master.apply(&mut next, 0.5);
        assert!(next.as_slices()[0].iter().all(|s| (*s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn a_master_strip_at_unity_leaves_the_bus_bit_identical() {
        let mut master = MasterStrip::new(SR, 1.0);
        let mut buffers = buffers_filled(8, 0.123_456_79);
        master.apply(&mut buffers, 1.0);
        assert!(buffers
            .as_slices()
            .iter()
            .all(|ch| ch.iter().all(|s| *s == 0.123_456_79)));
    }

    #[test]
    fn the_ramp_length_has_a_floor_for_absurd_sample_rates() {
        assert_eq!(param_ramp_frames(48_000.0), 480);
        assert_eq!(param_ramp_frames(44_100.0), 441);
        assert_eq!(param_ramp_frames(1_000.0), PARAM_RAMP_MIN_FRAMES);
        assert_eq!(param_ramp_frames(0.0), PARAM_RAMP_MIN_FRAMES);
        assert_eq!(param_ramp_frames(f64::NAN), PARAM_RAMP_MIN_FRAMES);
    }

    #[test]
    fn peaks_are_fixed_size_and_report_only_the_tracks_that_ran() {
        let mut peaks = TrackPeaks::empty(2);
        assert_eq!(peaks.count(), 2);
        assert_eq!(peaks.get(0), TrackPeak::SILENT);

        peaks.set(
            1,
            TrackPeak {
                peak_l: 0.5,
                peak_r: 0.25,
            },
        );
        assert_eq!(peaks.get(1).max(), 0.5);
        assert_eq!(peaks.as_slice().len(), 2);
        assert_eq!(peaks.get(5), TrackPeak::SILENT, "past the end");

        // A write past the array is ignored rather than panicking.
        peaks.set(
            MAX_TRACKS,
            TrackPeak {
                peak_l: 1.0,
                peak_r: 1.0,
            },
        );
        assert_eq!(peaks.count(), 2);
        assert_eq!(TrackPeaks::default(), TrackPeaks::SILENT);
        assert_eq!(TrackPeaks::empty(999).count(), MAX_TRACKS);
    }

    #[test]
    fn a_strip_id_maps_onto_its_own_row_of_the_send_matrix_and_never_anothers() {
        // The one thing that would be catastrophic and silent: track n and bus n
        // sharing a row, so moving a track's reverb send moved a bus's.
        let mut slots: Vec<usize> = (0..MAX_TRACKS)
            .map(|i| StripId::Track(i).slot().unwrap())
            .chain((0..MAX_BUSES).map(|i| StripId::Bus(i).slot().unwrap()))
            .collect();
        assert_eq!(slots.len(), MAX_SEND_SOURCES);
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), MAX_SEND_SOURCES, "two strips share a row");
        assert_eq!(*slots.last().unwrap(), MAX_SEND_SOURCES - 1);

        assert_eq!(StripId::Track(MAX_TRACKS).slot(), None);
        assert_eq!(StripId::Bus(MAX_BUSES).slot(), None);
        assert!(StripId::Bus(0).is_bus() && !StripId::Track(0).is_bus());
        assert_eq!(StripId::Bus(3).index(), 3);
        assert_eq!(StripId::Track(2).to_string(), "track 2");
        assert_eq!(StripId::Bus(2).to_string(), "bus 2");
    }

    #[test]
    fn a_fresh_send_matrix_is_unity_and_post_fader_and_refuses_nonsense() {
        let params = MixerParams::new();
        for source in [StripId::Track(0), StripId::Bus(7)] {
            assert_eq!(params.send_level(source, 0), 1.0);
            assert!(!params.send_pre(source, 0));
            assert_eq!(params.send_targets(source, 0), (0.0, 1.0));
        }

        params.set_send_level(StripId::Track(1), 2, 0.4);
        assert_eq!(params.send_level(StripId::Track(1), 2), 0.4);
        assert_eq!(
            params.send_level(StripId::Bus(1), 2),
            1.0,
            "bus 1 must not share track 1's cell"
        );

        params.set_send_level(StripId::Track(1), 2, 9.0);
        assert_eq!(
            params.send_level(StripId::Track(1), 2),
            MAX_SEND_LEVEL,
            "clamped to the headroom a send knob has"
        );
        params.set_send_level(StripId::Track(1), 2, f32::NAN);
        assert_eq!(params.send_level(StripId::Track(1), 2), 0.0);
        params.set_send_level(StripId::Track(1), 2, -1.0);
        assert_eq!(params.send_level(StripId::Track(1), 2), 0.0);

        // Out-of-range pairs are inert rather than panicking: the indices come
        // from a webview.
        params.set_send_level(StripId::Track(MAX_TRACKS), 0, 0.5);
        assert_eq!(params.send_level(StripId::Track(MAX_TRACKS), 0), 0.0);
        assert_eq!(params.send_targets(StripId::Bus(MAX_BUSES), 0), (0.0, 0.0));
        assert!(!params.send_pre(StripId::Track(0), MAX_BUSES));
    }

    #[test]
    fn the_pre_post_flag_hands_the_level_to_exactly_one_of_the_two_taps() {
        let params = MixerParams::new();
        params.set_send_level(StripId::Track(0), 1, 0.6);
        assert_eq!(params.send_targets(StripId::Track(0), 1), (0.0, 0.6));
        params.set_send_pre(StripId::Track(0), 1, true);
        assert_eq!(params.send_targets(StripId::Track(0), 1), (0.6, 0.0));
    }

    #[test]
    fn a_bus_strip_behaves_exactly_as_a_track_strip_does() {
        let params = MixerParams::new();
        for bus in 0..MAX_BUSES {
            assert_eq!(params.bus_gain(bus), 1.0);
            assert_eq!(params.bus_pan(bus), 0.0);
            assert!(!params.bus_muted(bus));
            assert_eq!(params.bus_target(bus), [1.0, 1.0]);
        }
        params.set_bus_gain(2, 0.5);
        params.set_bus_pan(2, -1.0);
        let target = params.bus_target(2);
        assert!((target[0] - 0.5 * pan_gains(-1.0)[0]).abs() < 1e-6);
        params.set_bus_muted(2, true);
        assert_eq!(params.bus_target(2), [0.0, 0.0]);

        params.set_bus_gain(2, f32::NAN);
        assert_eq!(params.bus_gain(2), 0.0);
        params.set_bus_pan(2, 9.0);
        assert_eq!(params.bus_pan(2), 1.0);

        params.set_bus_gain(MAX_BUSES, 0.5);
        assert_eq!(params.bus_gain(MAX_BUSES), 0.0);
        assert_eq!(params.bus_target(MAX_BUSES), [0.0, 0.0]);
    }

    #[test]
    fn writing_a_strip_is_bit_identical_to_summing_it_into_silence() {
        // What lets a post-fader send exist at all: the value the master gets is
        // the same whether it was accumulated in one pass or produced into a
        // scratch buffer and added afterwards.
        let src = buffers_filled(512, 0.371);
        let mut summed = silent(512);
        let mut written = silent(512);
        let mut a = TrackStrip::new(SR, [1.0, 1.0]);
        let mut b = TrackStrip::new(SR, [1.0, 1.0]);
        let peak_a = a.mix_into(&src, &mut summed, [0.25, 0.75]);
        let peak_b = b.apply_into(&src, &mut written, [0.25, 0.75]);
        assert_eq!(peak_a, peak_b);
        assert_eq!(summed.as_slices()[0], written.as_slices()[0]);
        assert_eq!(summed.as_slices()[1], written.as_slices()[1]);
    }

    #[test]
    fn writing_a_strip_replaces_whatever_was_in_the_destination() {
        let src = buffers_filled(8, 1.0);
        let mut dst = buffers_filled(8, 0.5);
        let mut strip = TrackStrip::new(SR, [1.0, 1.0]);
        strip.apply_into(&src, &mut dst, [1.0, 1.0]);
        assert!(dst.as_slices()[0].iter().all(|s| *s == 1.0));
    }

    #[test]
    fn the_send_headroom_is_the_documented_two_decibels() {
        assert!((max_send_db() - 1.9382).abs() < 0.001, "{}", max_send_db());
        assert_eq!(MAX_SEND_LEVEL, 1.25);
    }

    #[test]
    fn bus_peaks_are_the_same_machinery_as_track_peaks_with_their_own_limit() {
        let mut peaks = BusPeaks::empty(2);
        assert_eq!(peaks.count(), 2);
        peaks.set(
            1,
            TrackPeak {
                peak_l: 0.5,
                peak_r: 0.25,
            },
        );
        assert_eq!(peaks.get(1).max(), 0.5);
        peaks.set(
            MAX_BUSES,
            TrackPeak {
                peak_l: 1.0,
                peak_r: 1.0,
            },
        );
        assert_eq!(peaks.count(), 2, "a write past the array is ignored");
        assert_eq!(BusPeaks::empty(999).count(), MAX_BUSES);
        assert_eq!(BusPeaks::default(), BusPeaks::SILENT);
    }

    #[test]
    fn the_track_limit_matches_the_note_channels_a_track_can_be_addressed_by() {
        // A note channel *is* a track index; 16 channels, 16 tracks.
        const { assert!(MAX_TRACKS == 16) };
    }
}
