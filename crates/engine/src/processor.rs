//! The one interface every effect in an insert chain implements — built-in DSP and hosted VST3 alike.

/// Live readings per effect for its visualisation.
pub const METERS: usize = 32;

/// What the audio thread knows about the moment being processed.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessCtx {
    pub sample_rate: f32,
    pub bpm: f64,
    /// Playhead at the first sample of this block, in beats.
    pub beat: f64,
    pub playing: bool,
}

/// A stereo, in-place audio effect.
///
/// Real-time contract for [`Processor::process`]: no allocation, no locks that
/// a non-audio thread can hold for long, no I/O, no panics. Everything that
/// needs memory happens in [`Processor::prepare`], which is called **off** the
/// audio thread before the processor is handed to it.
pub trait Processor: Send {
    /// Allocate for `sample_rate` and blocks of at most `max_block` frames.
    fn prepare(&mut self, sample_rate: f32, max_block: usize);

    /// Process `l.len()` frames in place. `l.len() == r.len() <= max_block`.
    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &ProcessCtx);

    /// Apply a parameter set (built-in effects). Called on the audio thread,
    /// so it must not allocate either. Unknown ids are ignored.
    fn set_params(&mut self, _params: &ParamBlock) {}

    /// Forget tails and envelopes (transport jump, bypass toggled on).
    fn reset(&mut self) {}

    /// Current gain reduction in dB (≤ 0) for meters; 0 when not a dynamics processor.
    fn gain_reduction_db(&self) -> f32 {
        0.0
    }

    /// Up to 32 live readings for the effect's visualisation (pitch, envelope, …);
    /// meaning is per effect type, documented in docs/architecture.md.
    fn meters(&self, _out: &mut [f32; METERS]) {}

    /// Mono tap of the wet signal only, for effects whose interesting output is
    /// not the mix (reverb). `None` → analysers use the processor output.
    fn wet_tap(&self) -> Option<&[f32]> {
        None
    }
}

/// Parameters of a built-in effect, already resolved to numbers on the
/// non-audio side. Fixed size so it travels through a ring buffer by value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParamBlock {
    /// Built-in effect parameters, in the order of the type's parameter list (see `fx::params`).
    pub values: [f32; 8],
    /// EQ bands (unused slots have `on = false`).
    pub bands: [BandParams; 24],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BandParams {
    pub on: bool,
    /// 0 lowcut, 1 lowshelf, 2 peak, 3 highshelf, 4 highcut, 5 notch, 6 bandpass, 7 tilt
    pub kind: u8,
    pub f: f32,
    pub g: f32,
    pub q: f32,
    /// biquad stages for cut filters (1..4)
    pub stages: u8,
    /// 0 stereo, 1 left, 2 right, 3 mid, 4 side
    pub place: u8,
    pub dynamic: bool,
    pub range: f32,
    pub thr: f32,
}
