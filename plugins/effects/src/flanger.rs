//! `effects.flanger` -- a swept short delay with feedback.
//!
//! ```text
//!            ┌────────── feedback ──────────┐
//!   in ─┬───▶│ delay(manual + depth·½(1+sin φ)) │──┬──▶ × mix ─┐
//!       │    └──────────── × fb (soft clipped) ◀──┘            │
//!       └──────────────────── dry × (1 − mix) ────────────────▶ + ─▶ out
//! ```
//!
//! # A flanger is a comb filter you can hear moving
//! Summing a signal with a copy of itself delayed by `d` cancels every frequency
//! whose period divides evenly into `2d` -- notches at `(2k+1)/2d`, peaks at
//! `k/d`. Because those are *harmonically related* rather than octave spaced,
//! the ear hears one hollow resonance rather than a series of filters, and
//! sweeping `d` slides the whole comb, which is the jet-plane sound. The delay
//! has to be **short** -- a fraction of a millisecond to about ten -- because at
//! longer delays the notches crowd below the ear's ability to resolve them and
//! the effect becomes a chorus.
//!
//! # Feedback is bipolar, and that is not a gimmick
//! Positive feedback sharpens the peaks; negative feedback inverts the comb, so
//! the notches land where the peaks were -- including one at DC, which is the
//! thin, hollow "through-zero-ish" flange that the classic pedals with a polarity
//! switch are prized for. Only the feedback path is soft clipped, so the dry
//! signal written into the line stays exactly itself and a flanger at 0 %
//! feedback is a plain FIR comb a test can check against the arithmetic.
//!
//! # Manual and depth are the two halves of the sweep
//! `manual` is where the sweep starts and `depth` is how far up it goes: the
//! delay runs from `manual` to `manual + depth` and never below, so a musician
//! setting the shortest delay by ear does not have the depth knob take it
//! somewhere else. Both are smoothed over [`SWEEP_MS`] rather than the standard
//! 10 ms, because dragging a delay across its whole range in 10 ms is a pitch
//! transient rather than an edit.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode, RtNodeFactory,
    SmoothedParams, CHANNELS,
};

use crate::dsp::{soft_clip, usable_sample_rate, DelayLine, Lfo};
use crate::effect::{index_of, Bypass, BYPASS_PARAM, MIX_PARAM};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.flanger";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "Flanger";

/// One line for a tooltip.
pub const DESCRIPTION: &str =
    "A swept short delay with bipolar feedback -- the comb filter you can hear moving";

/// Longest delay either knob can reach, in milliseconds.
pub const MAX_DELAY_MS: f32 = 10.0;

/// How long the manual delay and the depth take to cross their whole range.
pub const SWEEP_MS: f32 = 150.0;

/// Largest feedback the knob offers, either polarity.
///
/// 95 %: the comb rings hard and still decays. The loop is soft clipped, so
/// this is a safety margin on the *sound* rather than on stability.
pub const MAX_FEEDBACK: f32 = 95.0;

/// What the flanger exposes.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::new("rate", "Rate", "Hz", 0.01, 10.0, 0.25, ParamCurve::Log),
    ParamDef::new(
        "depth",
        "Depth",
        "ms",
        0.0,
        MAX_DELAY_MS,
        3.0,
        ParamCurve::Linear,
    )
    .smoothed(SWEEP_MS),
    ParamDef::new(
        "delay",
        "Manual",
        "ms",
        0.1,
        MAX_DELAY_MS,
        1.0,
        ParamCurve::Log,
    )
    .smoothed(SWEEP_MS),
    ParamDef::new(
        "feedback",
        "Feedback",
        "%",
        -MAX_FEEDBACK,
        MAX_FEEDBACK,
        40.0,
        ParamCurve::Linear,
    ),
    ParamDef::new("spread", "Spread", "%", 0.0, 100.0, 0.0, ParamCurve::Linear),
    ParamDef::new(MIX_PARAM, "Mix", "%", 0.0, 100.0, 50.0, ParamCurve::Linear),
];

/// Parameter indices, resolved once off the audio thread.
#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    rate: usize,
    depth: usize,
    delay: usize,
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
            delay: index_of(block, "delay"),
            feedback: index_of(block, "feedback"),
            spread: index_of(block, "spread"),
            mix: index_of(block, MIX_PARAM),
        }
    }
}

/// The flanger node.
pub struct FlangerNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    lines: [DelayLine; CHANNELS],
    lfo: Lfo,
    rate_hz: f32,
    sample_rate: f32,
}

impl FlangerNode {
    /// **Non-RT**: allocates the two delay lines.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let index = Indices::new(&params);
        let rate = usable_sample_rate(sample_rate);
        let frames = ((2.0 * MAX_DELAY_MS + 1.0) * 0.001 * rate).ceil() as usize;
        let smoothed = SmoothedParams::new(params, sample_rate);
        let mut lfo = Lfo::new();
        lfo.set_rate(smoothed.value(index.rate), rate);
        Self {
            rate_hz: smoothed.value(index.rate),
            params: smoothed,
            index,
            bypass: Bypass::new(max_block, sample_rate),
            lines: [DelayLine::new(frames), DelayLine::new(frames)],
            lfo,
            sample_rate: rate,
        }
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        self.lfo.reset();
    }

    /// Delay in samples for one side, from the unipolar sweep.
    #[inline]
    fn delay_samples(&self, manual: f32, depth: f32, offset: f32) -> f32 {
        let sweep = 0.5 * (1.0 + self.lfo.sine_at(offset));
        (manual + depth * sweep) * 0.001 * self.sample_rate
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        let [left, right] = ctx.buffers.as_mut_slices();

        for frame in 0..frames {
            let rate = self.params.next(self.index.rate);
            let depth = self.params.next(self.index.depth);
            let manual = self.params.next(self.index.delay);
            let feedback = self.params.next(self.index.feedback) * 0.01;
            let spread = self.params.next(self.index.spread) * 0.01;
            let mix = self.params.next(self.index.mix) * 0.01;

            if rate != self.rate_hz {
                self.rate_hz = rate;
                self.lfo.set_rate(rate, self.sample_rate);
            }
            self.lfo.advance();

            let dry_l = left[frame];
            let dry_r = right[frame];

            // A quarter turn each way, so a fully spread flanger has one side
            // at the top of its sweep while the other is at the bottom. Half a
            // turn each way would put both sides at the *same* point of the
            // cycle -- `sin(φ+½) = sin(φ−½)` -- and collapse the width.
            let wet_l = self.lines[0].read(self.delay_samples(manual, depth, spread * 0.25));
            let wet_r = self.lines[1].read(self.delay_samples(manual, depth, -spread * 0.25));

            // Only the feedback term is clipped: at 0 % the line receives the
            // input unchanged, so the comb is exactly the one the arithmetic
            // predicts.
            self.lines[0].write(dry_l + soft_clip(wet_l * feedback));
            self.lines[1].write(dry_r + soft_clip(wet_r * feedback));

            left[frame] = dry_l * (1.0 - mix) + wet_l * mix;
            right[frame] = dry_r * (1.0 - mix) + wet_r * mix;
        }
    }
}

impl RtNode for FlangerNode {
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

/// Builds [`FlangerNode`]s.
#[derive(Debug, Default)]
pub struct FlangerFactory;

impl FlangerFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for FlangerFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for FlangerFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(FlangerNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{level_db_at, render_mono, render_stereo, silence, sine, Harness};

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&FlangerFactory::new(), SR, 512)
    }

    /// A flanger parked at a fixed 1 ms delay: the comb is a still picture.
    fn parked(feedback: f32, mix: f32) -> Harness {
        let mut h = harness();
        h.set("rate", 0.01);
        h.set("depth", 0.0);
        h.set("delay", 1.0);
        h.set("feedback", feedback);
        h.set("spread", 0.0);
        h.set(MIX_PARAM, mix);
        // The manual delay and depth glide over 150 ms; give them a quarter of a
        // second before anything is measured.
        render_mono(&mut h, &silence(24_000));
        h
    }

    /// Level of a steady tone through `h`, in dB, past its settling time.
    fn tone_db(h: &mut Harness, hz: f64) -> f64 {
        let signal = sine(hz, 48_000, SR, 0.5);
        render_mono(h, &signal);
        let out = render_mono(h, &signal);
        level_db_at(&out[24_000..], hz, SR)
    }

    #[test]
    fn the_notches_land_where_a_one_millisecond_comb_puts_them() {
        // 1 ms of delay at 50 % mix: cancellation at 500 Hz, 1500 Hz, 2500 Hz
        // and reinforcement at 1 kHz and 2 kHz. This is the flanger's whole
        // identity, so it is measured against the arithmetic rather than eyeballed.
        for notch in [500.0_f64, 1_500.0, 2_500.0] {
            let mut h = parked(0.0, 50.0);
            let level = tone_db(&mut h, notch);
            assert!(
                level < -30.0,
                "{notch} Hz should be a notch, came out at {level:.1} dB"
            );
        }
        for peak in [1_000.0_f64, 2_000.0] {
            let mut h = parked(0.0, 50.0);
            let level = tone_db(&mut h, peak);
            // Dry and wet in phase: 0.5 + 0.5 = the input, i.e. -6 dB of a 0.5
            // amplitude sine.
            assert!(
                (level - 20.0 * 0.5_f64.log10()).abs() < 0.5,
                "{peak} Hz should be untouched, came out at {level:.1} dB"
            );
        }
    }

    #[test]
    fn negative_feedback_moves_the_comb_onto_the_peaks() {
        // The polarity switch is a different filter, not a quieter one: 1 kHz is
        // a peak with positive feedback and a notch with negative.
        let mut positive = parked(60.0, 50.0);
        let mut negative = parked(-60.0, 50.0);
        let up = tone_db(&mut positive, 1_000.0);
        let down = tone_db(&mut negative, 1_000.0);
        assert!(
            up > down + 5.0,
            "inverting the feedback should move 1 kHz from peak to notch: \
             {up:.1} dB against {down:.1} dB"
        );
    }

    #[test]
    fn feedback_sharpens_the_resonance_rather_than_merely_raising_the_level() {
        // A resonant comb is *narrower*: the peak grows and the shoulder beside
        // it does not.
        let mut flat = parked(0.0, 50.0);
        let mut resonant = parked(90.0, 50.0);
        let flat_peak = tone_db(&mut flat, 1_000.0);
        let resonant_peak = tone_db(&mut resonant, 1_000.0);
        assert!(
            resonant_peak > flat_peak + 5.0,
            "feedback did not resonate: {resonant_peak:.1} against {flat_peak:.1}"
        );
        let mut flat = parked(0.0, 50.0);
        let mut resonant = parked(90.0, 50.0);
        let flat_side = tone_db(&mut flat, 750.0);
        let resonant_side = tone_db(&mut resonant, 750.0);
        assert!(
            resonant_peak - resonant_side > flat_peak - flat_side + 5.0,
            "the resonance is not narrower, only louder"
        );
    }

    #[test]
    fn the_sweep_moves_the_comb_across_the_spectrum() {
        // With the delay swept, a tone sitting on a notch cannot stay there: the
        // measured level at 500 Hz has to change over the cycle.
        let mut h = harness();
        // Half a hertz, so the four measurement windows below walk one eighth
        // of the cycle each: at a rate commensurate with the window length the
        // sweep would traverse the same delays in every window and the average
        // level would be identical while the comb was in fact moving.
        h.set("rate", 0.5);
        h.set("depth", 4.0);
        h.set("delay", 1.0);
        h.set("feedback", 0.0);
        h.set(MIX_PARAM, 50.0);
        let signal = sine(500.0, 48_000, SR, 0.5);
        render_mono(&mut h, &signal);
        let out = render_mono(&mut h, &signal);
        // Four windows across half a cycle of the sweep.
        let levels: Vec<f64> = (0..4)
            .map(|k| level_db_at(&out[k * 6_000..(k + 1) * 6_000], 500.0, SR))
            .collect();
        let spread = levels.iter().cloned().fold(f64::MIN, f64::max)
            - levels.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            spread > 6.0,
            "the comb is not moving: levels {levels:?} span only {spread:.1} dB"
        );
    }

    #[test]
    fn spread_puts_the_two_sides_at_opposite_points_of_the_sweep() {
        let mut h = harness();
        h.set("rate", 0.01);
        h.set("depth", 6.0);
        h.set("delay", 1.0);
        h.set("feedback", 0.0);
        h.set("spread", 100.0);
        h.set(MIX_PARAM, 100.0);
        render_mono(&mut h, &silence(24_000));

        let mut input = vec![[0.0_f32; 2]; 4_096];
        input[0] = [1.0, 1.0];
        let out = render_stereo(&mut h, &input);
        let first = |channel: usize| {
            out.iter()
                .position(|f| f[channel].abs() > 0.05)
                .unwrap_or(usize::MAX)
        };
        let gap = (first(0) as f32 - first(1) as f32).abs() / SR as f32 * 1_000.0;
        assert!(gap > 2.0, "the two sides are only {gap:.2} ms apart");
    }

    #[test]
    fn a_hard_driven_loop_saturates_instead_of_diverging() {
        let mut h = harness();
        h.set("feedback", MAX_FEEDBACK);
        h.set("delay", 0.5);
        h.set("depth", 1.0);
        h.set(MIX_PARAM, 100.0);
        let mut signal = sine(220.0, 48_000, SR, 0.9);
        signal.extend(silence(48_000));
        let out = render_mono(&mut h, &signal);
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(
            out.iter().fold(0.0_f32, |peak, s| peak.max(s.abs())) < 4.0,
            "the comb ran away"
        );
    }

    #[test]
    fn a_bypassed_flanger_is_the_input_again() {
        let mut h = harness();
        h.set(MIX_PARAM, 100.0);
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
