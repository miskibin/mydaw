//! `effects.phaser` -- a swept cascade of first-order all-pass stages.
//!
//! ```text
//!   in ─┬──▶(+)─▶ AP ─▶ AP ─▶ AP ─▶ AP ─┬─▶ × mix ─┐
//!       │     ▲   (corner = centre · 2^(depth·sin φ))│
//!       │     └────── × feedback ───────┘           │
//!       └──────────── dry × (1 − mix) ─────────────▶+ ─▶ out
//! ```
//!
//! # A phaser is not a flanger with a shorter delay
//! Both make notches; they are different filters. A flanger's comb comes from a
//! *delay*, so its notches are harmonically spaced and there are dozens of them.
//! A phaser's come from all-pass stages, which pass every frequency at full
//! level and only rotate its phase -- by `-2·atan(f/fc)` each. Adding the rotated
//! signal back to the dry one cancels wherever the total rotation reaches 180°,
//! so `N` stages give `N/2` notches at *non-harmonic* frequencies:
//! `fc·tan(π/2N)`, `fc·tan(3π/2N)`, and so on. Few, wide, unevenly spaced --
//! which is why a phaser sounds like breathing and a flanger like a jet.
//!
//! The stage count is therefore the character control, not a quality setting:
//! four stages is two notches (the classic pedal), eight is four (thick and
//! synthetic). It is a switch and not a knob because there is no such thing as
//! five and a half all-pass stages.
//!
//! # Feedback, and why it is one-sided here
//! Routing the cascade's output back to its input makes the notch shoulders
//! resonate, which is the difference between a subtle sweep and a vocal one. The
//! loop's gain is bounded by the all-pass magnitude, which is exactly one, so the
//! feedback term is soft clipped rather than allowed to run to `1/(1-k)` on
//! whatever frequency happens to arrive in phase.
//!
//! # Coefficients move at control rate
//! One `tan` per stage per sample would be most of the effect's cost, and the
//! corner it computes moves by a few hundredths of a percent between samples.
//! The cascade is re-aimed every [`CONTROL_INTERVAL`] samples instead, counted
//! against the node's own sample clock rather than the block boundary -- so the
//! output does not depend on the size of the buffer the device asks for, which
//! `tests/determinism.rs` checks across block sizes from 1 to 512.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode, RtNodeFactory,
    SmoothedParams, CHANNELS,
};

use crate::dsp::{soft_clip, usable_sample_rate, Lfo, OnePoleTpt};
use crate::effect::{index_of, Bypass, BYPASS_PARAM, MIX_PARAM};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.phaser";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "Phaser";

/// One line for a tooltip.
pub const DESCRIPTION: &str =
    "Four to eight swept all-pass stages with resonant feedback -- notches that breathe";

/// Stage counts the switch offers. Two notches, three, or four.
pub const STAGE_COUNTS: [usize; 3] = [4, 6, 8];

/// Most stages any position asks for, and the size of the fixed array.
pub const MAX_STAGES: usize = 8;

/// How far a fully-turned depth knob sweeps the corner, in octaves either way.
pub const MAX_SWEEP_OCTAVES: f32 = 2.0;

/// Largest feedback the knob offers.
pub const MAX_FEEDBACK: f32 = 90.0;

/// How long the centre frequency takes to cross its whole range.
///
/// Longer than the standard 10 ms ramp: the corner is the position of every
/// notch at once, and moving it across six octaves in ten milliseconds is a
/// sweep the ear reads as a click rather than as an edit. 150 ms is a gesture.
pub const SWEEP_MS: f32 = 150.0;

/// Samples between two re-aimings of the cascade.
///
/// 16 samples is a third of a millisecond at 48 kHz: three thousand updates a
/// second against a modulation that completes at most ten cycles in one.
pub const CONTROL_INTERVAL: usize = 16;

/// What the phaser exposes.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::new("rate", "Rate", "Hz", 0.01, 10.0, 0.5, ParamCurve::Log),
    ParamDef::new("depth", "Depth", "%", 0.0, 100.0, 70.0, ParamCurve::Linear),
    ParamDef::new(
        "freq",
        "Centre",
        "Hz",
        100.0,
        8_000.0,
        800.0,
        ParamCurve::Log,
    )
    .smoothed(SWEEP_MS),
    ParamDef::choice("stages", "Stages", 3, 0),
    ParamDef::new(
        "feedback",
        "Feedback",
        "%",
        0.0,
        MAX_FEEDBACK,
        30.0,
        ParamCurve::Linear,
    ),
    ParamDef::new("spread", "Spread", "%", 0.0, 100.0, 0.0, ParamCurve::Linear),
    ParamDef::new(MIX_PARAM, "Mix", "%", 0.0, 100.0, 50.0, ParamCurve::Linear),
];

/// The lowest notch of an `stages`-deep cascade cornered at `centre`, in Hz.
///
/// `fc · tan(π / 2N)` -- the frequency at which the cascade has rotated the
/// phase by half a turn. Public because the editor draws the response from the
/// same relation, and because a test should compare the audio against the
/// formula rather than against a second implementation of it.
pub fn first_notch_hz(centre: f32, stages: usize) -> f32 {
    let n = stages.max(1) as f32;
    centre * (std::f32::consts::PI / (2.0 * n)).tan()
}

/// Parameter indices, resolved once off the audio thread.
#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    rate: usize,
    depth: usize,
    freq: usize,
    stages: usize,
    feedback: usize,
    spread: usize,
    mix: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        Self {
            bypass: index_of(block, BYPASS_PARAM),
            rate: index_of(block, "rate"),
            depth: index_of(block, "depth"),
            freq: index_of(block, "freq"),
            stages: index_of(block, "stages"),
            feedback: index_of(block, "feedback"),
            spread: index_of(block, "spread"),
            mix: index_of(block, MIX_PARAM),
        }
    }
}

/// The phaser node.
pub struct PhaserNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    /// Every stage of both channels. Allocated at its maximum; the stage switch
    /// chooses how many of them are walked.
    stages: [[OnePoleTpt; MAX_STAGES]; CHANNELS],
    /// The cascade's last output per channel, for the feedback path.
    last: [f32; CHANNELS],
    lfo: Lfo,
    rate_hz: f32,
    sample_rate: f32,
    /// Samples since the last re-aiming. Never reset by a block boundary.
    control: usize,
}

impl PhaserNode {
    /// **Non-RT**: allocates the bypass tap.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let index = Indices::new(&params);
        let rate = usable_sample_rate(sample_rate);
        let smoothed = SmoothedParams::new(params, sample_rate);
        let mut lfo = Lfo::new();
        lfo.set_rate(smoothed.value(index.rate), rate);
        Self {
            rate_hz: smoothed.value(index.rate),
            params: smoothed,
            index,
            bypass: Bypass::new(max_block, sample_rate),
            stages: [[OnePoleTpt::new(); MAX_STAGES]; CHANNELS],
            last: [0.0; CHANNELS],
            lfo,
            sample_rate: rate,
            // Due immediately, so the first sample runs through a cascade that
            // has been aimed at the settings rather than at its defaults.
            control: 0,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.stages {
            for stage in channel.iter_mut() {
                stage.reset();
            }
        }
        self.last = [0.0; CHANNELS];
        self.lfo.reset();
        self.control = 0;
    }

    /// Point one channel's stages at the corner the LFO has moved them to.
    fn aim(&mut self, channel: usize, centre: f32, octaves: f32, offset: f32) {
        let corner = centre * (octaves * self.lfo.sine_at(offset)).exp2();
        let coefficient = OnePoleTpt::coefficient_for(corner, self.sample_rate);
        for stage in &mut self.stages[channel] {
            stage.set_coefficient(coefficient);
        }
    }

    /// One sample through `count` all-pass stages of `channel`.
    ///
    /// A first-order all-pass is `2·lowpass − input`: the same TPT one-pole the
    /// rest of the pack uses, read as `LP − HP` instead of as `LP`.
    #[inline]
    fn cascade(&mut self, channel: usize, input: f32, count: usize) -> f32 {
        let mut x = input;
        for stage in self.stages[channel].iter_mut().take(count) {
            x = 2.0 * stage.low(x) - x;
        }
        x
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        let count = STAGE_COUNTS[self
            .params
            .choice(self.index.stages)
            .min(STAGE_COUNTS.len() - 1)];
        let [left, right] = ctx.buffers.as_mut_slices();

        for frame in 0..frames {
            let rate = self.params.next(self.index.rate);
            let depth = self.params.next(self.index.depth) * 0.01;
            let centre = self.params.next(self.index.freq);
            let feedback = self.params.next(self.index.feedback) * 0.01;
            let spread = self.params.next(self.index.spread) * 0.01;
            let mix = self.params.next(self.index.mix) * 0.01;

            if rate != self.rate_hz {
                self.rate_hz = rate;
                self.lfo.set_rate(rate, self.sample_rate);
            }
            self.lfo.advance();

            if self.control == 0 {
                let octaves = depth * MAX_SWEEP_OCTAVES;
                self.aim(0, centre, octaves, spread * 0.25);
                self.aim(1, centre, octaves, -spread * 0.25);
            }
            self.control = (self.control + 1) % CONTROL_INTERVAL;

            let dry_l = left[frame];
            let dry_r = right[frame];

            let wet_l = self.cascade(0, dry_l + soft_clip(self.last[0] * feedback), count);
            let wet_r = self.cascade(1, dry_r + soft_clip(self.last[1] * feedback), count);
            self.last = [wet_l, wet_r];

            left[frame] = dry_l * (1.0 - mix) + wet_l * mix;
            right[frame] = dry_r * (1.0 - mix) + wet_r * mix;
        }
    }
}

impl RtNode for PhaserNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        self.params.begin_block();
        let bypassed = self.params.choice(self.index.bypass) != 0;
        let state = self.bypass.begin(ctx, bypassed);

        if state.processes() {
            if state.resets() {
                self.reset();
            }
            self.render(ctx);
        } else {
            self.params.skip_all(ctx.frames);
        }

        self.bypass.blend(ctx, state, bypassed);
    }
}

/// Builds [`PhaserNode`]s.
#[derive(Debug, Default)]
pub struct PhaserFactory;

impl PhaserFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for PhaserFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for PhaserFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(PhaserNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{level_db_at, render_mono, render_stereo, rms_mono, sine, Harness};

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&PhaserFactory::new(), SR, 512)
    }

    /// A phaser with its sweep stopped, so the response is a still picture.
    fn parked(stages: f32, feedback: f32) -> Harness {
        let mut h = harness();
        h.set("rate", 0.01);
        h.set("depth", 0.0);
        h.set("freq", 800.0);
        h.set("stages", stages);
        h.set("feedback", feedback);
        h.set("spread", 0.0);
        h.set(MIX_PARAM, 50.0);
        h
    }

    /// Level of a steady tone through `h`, in dB, past its settling time.
    fn tone_db(h: &mut Harness, hz: f64) -> f64 {
        let signal = sine(hz, 48_000, SR, 0.5);
        render_mono(h, &signal);
        let out = render_mono(h, &signal);
        level_db_at(&out[24_000..], hz, SR)
    }

    /// The notch a digital cascade puts where the analogue formula says
    /// `fc·tan(π/2N)`, with the bilinear transform's frequency warping applied.
    fn warped_notch(centre: f64, stages: usize) -> f64 {
        let g = (std::f64::consts::PI * centre / SR).tan();
        let ratio = (std::f64::consts::PI / (2.0 * stages as f64)).tan();
        SR / std::f64::consts::PI * (g * ratio).atan()
    }

    #[test]
    fn the_first_notch_lands_where_the_all_pass_phase_reaches_half_a_turn() {
        // Four stages cornered at 800 Hz cancel at 800·tan(π/8) ≈ 331 Hz. This
        // is the phaser's identity: measured against the formula, not against a
        // remembered number.
        let notch = warped_notch(800.0, 4);
        assert!((notch - 331.0).abs() < 3.0, "{notch}");
        let mut h = parked(0.0, 0.0);
        let level = tone_db(&mut h, notch);
        assert!(level < -26.0, "the notch is only {level:.1} dB deep");

        // The corner itself is a full turn of phase for four stages, so the wet
        // path arrives in phase and 800 Hz comes back untouched.
        let mut h = parked(0.0, 0.0);
        let at_corner = tone_db(&mut h, 800.0);
        assert!(
            (at_corner - 20.0 * 0.5_f64.log10()).abs() < 0.6,
            "800 Hz should be untouched, came out at {at_corner:.1} dB"
        );
    }

    #[test]
    fn more_stages_means_more_notches_and_they_move_down() {
        // Six and eight stages rotate faster, so their first notch is lower --
        // and each pair of stages adds one more notch above it.
        for (choice, stages) in [(0.0_f32, 4_usize), (1.0, 6), (2.0, 8)] {
            let notch = warped_notch(800.0, stages);
            let mut h = parked(choice, 0.0);
            let level = tone_db(&mut h, notch);
            assert!(
                level < -20.0,
                "{stages} stages: the notch at {notch:.0} Hz is only {level:.1} dB deep"
            );
        }
        assert!(warped_notch(800.0, 8) < warped_notch(800.0, 4));
        // The second notch of an eight-stage cascade is at 3π/2N, and it is
        // *not* an octave or a harmonic of the first: that is what distinguishes
        // the sound from a flanger's.
        let first = warped_notch(800.0, 8);
        let g = (std::f64::consts::PI * 800.0 / SR).tan();
        let second =
            SR / std::f64::consts::PI * (g * (3.0 * std::f64::consts::PI / 16.0).tan()).atan();
        assert!(
            second / first > 2.5 && second / first < 4.0,
            "{first} {second}"
        );
        let mut h = parked(2.0, 0.0);
        assert!(tone_db(&mut h, second) < -18.0);
    }

    #[test]
    fn the_centre_knob_moves_the_notches_with_it() {
        let mut low = parked(0.0, 0.0);
        low.set("freq", 400.0);
        let level = tone_db(&mut low, warped_notch(400.0, 4));
        assert!(level < -26.0, "{level:.1} dB");

        // ...and the notch that *was* there is not any more.
        let mut low = parked(0.0, 0.0);
        low.set("freq", 400.0);
        let moved = tone_db(&mut low, warped_notch(800.0, 4));
        assert!(moved > -12.0, "the notch did not move: {moved:.1} dB");
    }

    #[test]
    fn feedback_makes_the_shoulders_resonate() {
        let mut flat = parked(0.0, 0.0);
        let mut resonant = parked(0.0, MAX_FEEDBACK);
        // At the corner itself: four stages have rotated a full turn there, so
        // the feedback arrives in phase and the loop gain is 1/(1−k). That is
        // where a phaser's resonance lives, between the notches either side.
        let probe = 800.0;
        let dry = tone_db(&mut flat, probe);
        let wet = tone_db(&mut resonant, probe);
        assert!(
            wet > dry + 4.0,
            "feedback did nothing at {probe:.0} Hz: {wet:.1} against {dry:.1}"
        );
    }

    #[test]
    fn the_sweep_moves_the_notches_across_the_spectrum() {
        let mut h = harness();
        // Half a hertz: see the flanger's equivalent test for why a rate
        // commensurate with the measurement window hides the movement.
        h.set("rate", 0.5);
        h.set("depth", 100.0);
        h.set("freq", 800.0);
        h.set("feedback", 0.0);
        h.set(MIX_PARAM, 50.0);
        let probe = 331.0;
        let signal = sine(probe, 48_000, SR, 0.5);
        render_mono(&mut h, &signal);
        let out = render_mono(&mut h, &signal);
        let levels: Vec<f64> = (0..4)
            .map(|k| level_db_at(&out[k * 6_000..(k + 1) * 6_000], probe, SR))
            .collect();
        let spread = levels.iter().cloned().fold(f64::MIN, f64::max)
            - levels.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread > 6.0, "the sweep is not moving: {levels:?}");
    }

    #[test]
    fn the_all_pass_cascade_alone_changes_the_phase_and_not_the_level() {
        // 100 % wet with no feedback is a pure all-pass: the ear hears nothing,
        // and the meter says why -- same level, different waveform.
        let mut h = parked(2.0, 0.0);
        h.set(MIX_PARAM, 100.0);
        let signal = sine(1_000.0, 48_000, SR, 0.5);
        render_mono(&mut h, &signal);
        let out = render_mono(&mut h, &signal);
        let level = rms_mono(&out[24_000..]);
        let reference = rms_mono(&signal[24_000..]);
        assert!(
            (20.0 * (level / reference).log10()).abs() < 0.2,
            "an all-pass changed the level: {level:.4} against {reference:.4}"
        );
        // ...and it really did rotate the phase, or the test above proves nothing.
        let shifted: f32 = out[24_000..]
            .iter()
            .zip(&signal[24_000..])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(shifted > 0.1, "nothing was rotated at all");
    }

    #[test]
    fn a_bypassed_phaser_is_the_input_again() {
        let mut h = harness();
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 60.0);
        let input: Vec<[f32; 2]> = (0..8_192)
            .map(|n| [(n as f32 * 0.03).sin() * 0.5, (n as f32 * 0.05).sin() * 0.5])
            .collect();
        render_stereo(&mut h, &input);
        h.set("bypass", 1.0);
        render_stereo(&mut h, &input);
        let out = render_stereo(&mut h, &input);
        assert_eq!(out, input, "a settled bypass is the input, bit for bit");
    }
}
