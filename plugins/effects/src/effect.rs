//! What every effect in the pack has in common: a dry tap, a click-free bypass,
//! and the handful of parameter ids that mean the same thing in all five.
//!
//! # Why bypass is a crossfade and not a branch
//! The VST3 specification puts the responsibility on the plugin: it "must take
//! care of **artifact-free switching (ramping/fade-in/fade-out, parallel
//! processing or algorithm changes)**", and every vendor that markets a bypass
//! markets it as *soft*. The failure mode is well documented by users on the
//! other side of it -- "horrible click sounds when bypassing/unbypassing" -- and
//! it is not subtle: switching a +12 dB shelf out mid-note is a step change in
//! the waveform, which is a click by definition.
//!
//! So `bypass` is a switch that drives a **10 ms equal-gain crossfade** between
//! the processed signal and a copy of the input taken before the effect touched
//! it. Equal gain rather than equal power because the two signals are the same
//! signal: they are correlated, so their sum is coherent and a `√` law would
//! bulge by 3 dB in the middle.
//!
//! # Two properties the crossfade has to have
//!
//! **Bit-transparent once settled.** A bypassed effect is not "very nearly the
//! input"; it *is* the input, sample for sample, because the settled path copies
//! the dry buffer back rather than multiplying it by a coefficient that happens
//! to be 1.0. `tests/bypass.rs` asserts equality, not a tolerance.
//!
//! **Free once settled.** A fully bypassed effect skips its DSP entirely. The
//! cost is that its internal state -- filter memory, a delay line, an envelope --
//! goes stale, so the transition *out* of full bypass resets it and fades in
//! from silence over the same 10 ms. That is the right trade: a delay coming back
//! with a fresh tail is what a user expects, and replaying eight seconds of audio
//! from before they switched it off is not.

use mydaw_rt_core::{smoothing_frames, ProcessCtx, CHANNELS};

/// How long a bypass crossfade takes, in milliseconds.
///
/// The same 10 ms as every other parameter ramp in the engine. See
/// [`PARAM_RAMP_SECONDS`](mydaw_rt_core::PARAM_RAMP_SECONDS).
pub const BYPASS_FADE_MS: f32 = 10.0;

/// Parameter id every effect in the pack carries.
///
/// A single spelling, so the shell can offer "bypass this insert" without asking
/// what kind of insert it is.
pub const BYPASS_PARAM: &str = "bypass";

/// Parameter id for a dry/wet control, on the effects that have one.
///
/// Not all of them do, and that is a decision rather than an omission: the
/// research is that a mix knob belongs on non-linear, dynamic and time-based
/// effects and is meaningless-to-harmful on gain, pan and EQ.
pub const MIX_PARAM: &str = "mix";

/// What a node should do with this block, having asked the bypass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassState {
    /// Run the DSP normally. Nothing is fading.
    Active,
    /// Run the DSP; the output will be crossfaded against the dry tap.
    Fading,
    /// Skip the DSP. The dry tap will be written back unchanged.
    Settled,
    /// Run the DSP, but reset it first: the effect is coming back from a
    /// completely bypassed state and its memory is stale.
    Resuming,
}

impl BypassState {
    /// Whether the node should run its DSP at all.
    pub const fn processes(self) -> bool {
        !matches!(self, Self::Settled)
    }

    /// Whether the node should clear its filters, delay lines and envelopes
    /// before processing.
    pub const fn resets(self) -> bool {
        matches!(self, Self::Resuming)
    }
}

/// A copy of the input, and the crossfade that puts it back.
///
/// Owned by each effect node and sized to `max_block` at construction, so
/// nothing here allocates on the audio thread.
#[derive(Debug)]
pub struct Bypass {
    dry: [Box<[f32]>; CHANNELS],
    /// 0.0 fully processed, 1.0 fully bypassed.
    position: f32,
    /// Per-sample step of a full-distance fade. Never zero.
    step: f32,
    /// Whether the last block ended fully bypassed with the DSP skipped.
    was_settled: bool,
}

impl Bypass {
    /// A bypass for blocks of up to `max_block` frames.
    ///
    /// **Non-RT**: allocates the dry tap.
    pub fn new(max_block: usize, sample_rate: f64) -> Self {
        let frames = max_block.max(1);
        let ramp = smoothing_frames(BYPASS_FADE_MS, sample_rate).max(1);
        Self {
            dry: std::array::from_fn(|_| vec![0.0; frames].into_boxed_slice()),
            position: 0.0,
            step: 1.0 / ramp as f32,
            // A fresh node is *not* settled-bypassed: it starts processing, so
            // an effect inserted while `bypass` is on still fades correctly.
            was_settled: false,
        }
    }

    /// Whether the crossfade has finished moving.
    pub fn is_settled(&self) -> bool {
        self.position <= 0.0 || self.position >= 1.0
    }

    /// The crossfade position: 0 processed, 1 bypassed. Diagnostics and tests.
    pub const fn position(&self) -> f32 {
        self.position
    }

    /// Take the dry copy and decide what the node should do this block.
    ///
    /// Call this **first**, before any processing, and pass the result to
    /// [`blend`](Self::blend) afterwards.
    pub fn begin(&mut self, ctx: &ProcessCtx, bypassed: bool) -> BypassState {
        let frames = ctx.frames.min(self.dry[0].len());
        let input = ctx.buffers.as_slices();
        for (tap, source) in self.dry.iter_mut().zip(input) {
            tap[..frames].copy_from_slice(&source[..frames]);
        }

        if bypassed && self.position >= 1.0 {
            self.was_settled = true;
            return BypassState::Settled;
        }
        let resuming = self.was_settled;
        self.was_settled = false;
        if resuming {
            BypassState::Resuming
        } else if bypassed || self.position > 0.0 {
            BypassState::Fading
        } else {
            BypassState::Active
        }
    }

    /// Crossfade the processed buffer against the dry tap and advance the fade.
    ///
    /// `state` must be the value [`begin`](Self::begin) returned for this block.
    ///
    /// # RT safety
    /// One multiply-add per sample per channel while fading, a `copy_from_slice`
    /// when settled-bypassed, and nothing at all when settled-active.
    pub fn blend(&mut self, ctx: &mut ProcessCtx, state: BypassState, bypassed: bool) {
        let frames = ctx.frames.min(self.dry[0].len());

        if state == BypassState::Settled {
            // Bit-transparent: the input written back, not multiplied by a
            // coefficient that rounds to one.
            let output = ctx.buffers.as_mut_slices();
            for (target, tap) in output.into_iter().zip(&self.dry) {
                target[..frames].copy_from_slice(&tap[..frames]);
            }
            return;
        }

        let target = if bypassed { 1.0 } else { 0.0 };
        if self.position == target {
            // Fully processed and staying that way: the common case costs
            // nothing, and the buffer already holds the wet signal.
            if target == 0.0 {
                return;
            }
        }

        let mut position = self.position;
        let step = self.step;
        let [out_l, out_r] = ctx.buffers.as_mut_slices();
        let [dry_l, dry_r] = &self.dry;
        for frame in 0..frames {
            if position < target {
                position = (position + step).min(target);
            } else if position > target {
                position = (position - step).max(target);
            }
            let wet = 1.0 - position;
            out_l[frame] = out_l[frame] * wet + dry_l[frame] * position;
            out_r[frame] = out_r[frame] * wet + dry_r[frame] * position;
        }
        self.position = position;
    }

    /// Land the fade immediately. For a node that has just been built into a new
    /// graph and should not fade in.
    pub fn settle(&mut self, bypassed: bool) {
        self.position = if bypassed { 1.0 } else { 0.0 };
        self.was_settled = bypassed;
    }

    /// The dry tap for `channel`, for an effect whose mix knob wants it.
    pub fn dry(&self, channel: usize) -> &[f32] {
        &self.dry[channel.min(CHANNELS - 1)]
    }
}

/// Resolve a parameter id to its index at construction time, panicking on a
/// typo.
///
/// A node addresses parameters by index on the audio thread; the lookup happens
/// once, here, off it. The panic is deliberate and unreachable at runtime: the
/// id and the declaration are in the same file, so a mismatch is a compile-time
/// mistake that this turns into a loud construction-time one rather than a
/// silent parameter that never moves.
///
/// **Non-RT**: called from a factory.
pub fn index_of(block: &mydaw_rt_core::ParamBlock, id: &'static str) -> usize {
    block
        .index_of(id)
        .unwrap_or_else(|| panic!("parameter '{id}' is not declared by this node type"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_rt_core::{AudioBuffers, TransportSnapshot};

    const SR: f64 = 48_000.0;

    /// A stand-in effect: halving is exact in binary floating point, so a test
    /// can tell processed from dry without a tolerance.
    fn halve(ctx: &mut ProcessCtx) {
        for channel in ctx.buffers.as_mut_slices() {
            for sample in channel.iter_mut() {
                *sample *= 0.5;
            }
        }
    }

    /// One block of `value` through the bypass, returning the left channel.
    fn render(bypass: &mut Bypass, frames: usize, value: f32, bypassed: bool) -> Vec<f32> {
        let mut buffers = AudioBuffers::new(frames);
        for channel in buffers.as_mut_slices() {
            channel.fill(value);
        }
        {
            let mut ctx = ProcessCtx::new(&mut buffers, SR, frames, TransportSnapshot::default());
            let state = bypass.begin(&ctx, bypassed);
            if state.processes() {
                halve(&mut ctx);
            }
            bypass.blend(&mut ctx, state, bypassed);
        }
        buffers.as_slices()[0].to_vec()
    }

    #[test]
    fn an_unbypassed_effect_is_simply_processed() {
        let mut bypass = Bypass::new(64, SR);
        let out = render(&mut bypass, 64, 1.0, false);
        assert!(out.iter().all(|s| *s == 0.5));
        assert!(bypass.is_settled());
        assert_eq!(bypass.position(), 0.0);
    }

    #[test]
    fn a_settled_bypass_is_bit_transparent_and_skips_the_dsp() {
        let mut bypass = Bypass::new(1_024, SR);
        // Two blocks: the first covers the 480-frame fade, the second is the
        // settled state the claim is about.
        render(&mut bypass, 1_024, 1.0, true);
        assert_eq!(bypass.position(), 1.0);

        let mut buffers = AudioBuffers::new(8);
        for channel in buffers.as_mut_slices() {
            channel.fill(0.123_456_79);
        }
        let mut processed = false;
        {
            let mut ctx = ProcessCtx::new(&mut buffers, SR, 8, TransportSnapshot::default());
            let state = bypass.begin(&ctx, true);
            assert_eq!(state, BypassState::Settled);
            if state.processes() {
                processed = true;
                halve(&mut ctx);
            }
            bypass.blend(&mut ctx, state, true);
        }
        assert!(!processed, "a settled bypass must not run the DSP");
        assert!(
            buffers.as_slices()[0].iter().all(|s| *s == 0.123_456_79),
            "a bypassed effect is the input, not nearly the input"
        );
    }

    #[test]
    fn engaging_bypass_is_a_fade_and_never_a_step() {
        // The zipper test. A constant input means the output *is* the crossfade
        // coefficient, so a discontinuity in it is a discontinuity in the audio.
        let mut bypass = Bypass::new(2_048, SR);
        render(&mut bypass, 2_048, 1.0, false);
        let out = render(&mut bypass, 2_048, 1.0, true);

        // The whole distance is 0.5 (from the halved signal to the dry one) over
        // 480 frames.
        let limit = 0.5 / 480.0 + 1e-6;
        let worst = out
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(worst <= limit, "the bypass stepped by {worst}");
        assert!((out[0] - 0.5).abs() < limit, "it must start where it was");
        assert!((out[2_047] - 1.0).abs() < 1e-6, "and it must arrive");
    }

    #[test]
    fn a_fade_longer_than_the_block_continues_on_the_next_one() {
        let mut bypass = Bypass::new(64, SR);
        let first = render(&mut bypass, 64, 1.0, true);
        assert!(!bypass.is_settled(), "64 frames is not a 480-frame fade");
        let second = render(&mut bypass, 64, 1.0, true);
        let boundary = (second[0] - first[63]).abs();
        assert!(
            boundary <= 0.5 / 480.0 + 1e-6,
            "block boundary step {boundary}"
        );
    }

    #[test]
    fn coming_back_from_a_settled_bypass_asks_the_node_to_reset() {
        // A delay that had eight seconds of tail in it when the user switched it
        // off must not replay it when they switch it back on.
        let mut bypass = Bypass::new(1_024, SR);
        render(&mut bypass, 1_024, 1.0, true);
        render(&mut bypass, 64, 1.0, true);

        let mut buffers = AudioBuffers::new(64);
        let ctx = ProcessCtx::new(&mut buffers, SR, 64, TransportSnapshot::default());
        let state = bypass.begin(&ctx, false);
        assert_eq!(state, BypassState::Resuming);
        assert!(state.resets());
        assert!(state.processes());
    }

    #[test]
    fn a_bypass_settled_at_construction_does_not_fade_in() {
        // What a graph rebuild needs: a node built into a new graph picks the
        // setting up where the old one left it.
        let mut bypass = Bypass::new(64, SR);
        bypass.settle(true);
        let mut buffers = AudioBuffers::new(8);
        for channel in buffers.as_mut_slices() {
            channel.fill(1.0);
        }
        let ctx = ProcessCtx::new(&mut buffers, SR, 8, TransportSnapshot::default());
        assert_eq!(bypass.begin(&ctx, true), BypassState::Settled);
    }

    #[test]
    fn a_block_larger_than_the_tap_truncates_rather_than_reading_past_it() {
        // The dry tap is sized to `max_block`; a device that asks for more is a
        // bug elsewhere, and the audio thread is the wrong place to discover it.
        let mut bypass = Bypass::new(16, SR);
        let out = render(&mut bypass, 64, 1.0, false);
        assert_eq!(out.len(), 64);
        assert!(out[..16].iter().all(|s| *s == 0.5));
    }
}

/// A decibel parameter, cached as the linear gain it means.
///
/// `10^(dB/20)` is an `exp2` and a multiply -- cheap, but not cheap enough to
/// pay for on every sample of every channel of every effect when the answer has
/// not changed. A knob that is not moving is the overwhelmingly common case, so
/// the conversion happens only when the smoothed value actually steps, which is
/// during the 10 ms after somebody touches it and never otherwise.
#[derive(Debug, Clone, Copy)]
pub struct CachedDb {
    index: usize,
    db: f32,
    linear: f32,
}

impl CachedDb {
    /// A cache for parameter `index`, primed from `params`.
    pub fn new(index: usize, params: &mydaw_rt_core::SmoothedParams) -> Self {
        let db = params.value(index);
        Self {
            index,
            db,
            linear: crate::dsp::db_to_linear(db),
        }
    }

    /// Advance the parameter one sample and return the linear gain.
    #[inline]
    pub fn next(&mut self, params: &mut mydaw_rt_core::SmoothedParams) -> f32 {
        let db = params.next(self.index);
        if db != self.db {
            self.db = db;
            self.linear = crate::dsp::db_to_linear(db);
        }
        self.linear
    }

    /// The linear gain in force, without advancing.
    pub const fn linear(&self) -> f32 {
        self.linear
    }

    /// The parameter index this cache watches.
    pub const fn index(&self) -> usize {
        self.index
    }
}
