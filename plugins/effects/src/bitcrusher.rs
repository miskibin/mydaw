//! `effects.bitcrusher` -- bit-depth reduction, sample-rate decimation and a
//! lo-fi filter.
//!
//! ```text
//!   in ─┬─▶ quantise to 2^bits levels ─▶ sample & hold every N ─▶ LP ─┬─▶ mix ─▶ out
//!       └────────────────────────── dry ────────────────────────────┘
//! ```
//!
//! # Two different kinds of damage, and why both are here
//! A converter has two resolutions and they fail in unrelated ways. **Bit
//! depth** is amplitude resolution: rounding every sample onto a coarse grid
//! adds an error that correlates with the signal, which is why a crushed quiet
//! passage sounds gritty and a crushed loud one barely does. **Sample rate** is
//! time resolution: holding one sample for `N` periods is a zero-order hold,
//! whose spectrum is the input convolved with a comb of images at multiples of
//! `fs/N` -- the metallic ring of a cheap sampler, and *not* a filter, because
//! nothing here removes the images. That aliasing is the effect. Offering only
//! one of the two would make the other unreachable, and they are the two knobs
//! every lo-fi unit (Redux, Decimator, Bitcrusher) puts side by side.
//!
//! # The decimator holds a fraction, not an integer
//! The hold interval is a phase accumulator stepping by `1/N` rather than a
//! counter comparing against a whole number, so `N` can pass through 8.5 while
//! the knob is being dragged instead of jumping from 8 to 9. The knob still
//! *lands* on integers -- it is stepped -- but the 10 ms it spends between two
//! of them is a glide rather than a click.
//!
//! # The filter is after the crusher and defaults to off
//! A one-pole lowpass following the hold is the "age" control: it takes the top
//! off the images without pretending to reconstruct anything, which is exactly
//! what an old sampler's output stage did. At the top of its range it is
//! switched out rather than run at 20 kHz, so a fresh bitcrusher is a
//! bit-transparent pass-through -- the pack's rule for every effect.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode, RtNodeFactory,
    SmoothedParams, CHANNELS,
};

use crate::dsp::{quantise, usable_sample_rate, OnePoleTpt};
use crate::effect::{index_of, Bypass, BYPASS_PARAM, MIX_PARAM};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.bitcrusher";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "Bitcrusher";

/// One line for a tooltip.
pub const DESCRIPTION: &str =
    "Bit-depth reduction, sample-rate decimation and a lo-fi filter, with a dry blend";

/// Top of the bit-depth knob, and the position at which the quantiser is
/// switched out entirely.
///
/// Twenty-four bits is a step of 120 nV of full scale -- below the noise floor of
/// any converter and of `f32` arithmetic itself -- so rounding onto that grid
/// would be an operation nobody can hear that still costs the effect its
/// transparency claim. The knob's top position therefore means *off*.
pub const MAX_BITS: f32 = 24.0;

/// Most samples a single value is held for.
///
/// 64 puts the hold rate at 750 Hz on a 48 kHz device, which is well inside the
/// audio band and about as far as the effect stays a pitch rather than a rumble.
pub const MAX_DOWNSAMPLE: f32 = 64.0;

/// Top of the lo-fi filter, and the position at which it is switched out.
pub const MAX_TONE_HZ: f32 = 20_000.0;

/// What the bitcrusher exposes.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    // Stepped, so the knob lands on whole bits, but still smoothed: the 10 ms
    // between two depths is a glide in the quantiser's step size rather than a
    // discontinuity in the waveform.
    ParamDef::new(
        "bits",
        "Bit Depth",
        "bits",
        1.0,
        MAX_BITS,
        MAX_BITS,
        ParamCurve::Linear,
    )
    .stepped(1.0),
    ParamDef::new(
        "downsample",
        "Downsample",
        "x",
        1.0,
        MAX_DOWNSAMPLE,
        1.0,
        ParamCurve::Log,
    )
    .stepped(1.0),
    ParamDef::new(
        "tone",
        "Filter",
        "Hz",
        200.0,
        MAX_TONE_HZ,
        MAX_TONE_HZ,
        ParamCurve::Log,
    )
    // Forty milliseconds rather than ten: a one-pole dragged from 200 Hz to
    // 20 kHz slides the phase of everything below it, and doing that in a
    // tenth of the time is a chirp rather than a filter sweep.
    .smoothed(40.0),
    ParamDef::new(MIX_PARAM, "Mix", "%", 0.0, 100.0, 100.0, ParamCurve::Linear),
];

/// Parameter indices, resolved once off the audio thread.
#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    bits: usize,
    downsample: usize,
    tone: usize,
    mix: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        Self {
            bypass: index_of(block, BYPASS_PARAM),
            bits: index_of(block, "bits"),
            downsample: index_of(block, "downsample"),
            tone: index_of(block, "tone"),
            mix: index_of(block, MIX_PARAM),
        }
    }
}

/// The bitcrusher node.
pub struct BitcrusherNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    sample_rate: f32,
    /// The one lowpass per channel that follows the hold.
    tone: [OnePoleTpt; CHANNELS],
    /// Last cutoff the coefficient was computed for; `NaN` forces the first one.
    tone_hz: f32,
    /// Sample-and-hold phase, in `0..1` of one hold period.
    hold_phase: f32,
    /// The value being held, per channel.
    held: [f32; CHANNELS],
}

impl BitcrusherNode {
    /// **Non-RT**: allocates the bypass tap.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let index = Indices::new(&params);
        Self {
            params: SmoothedParams::new(params, sample_rate),
            index,
            bypass: Bypass::new(max_block, sample_rate),
            sample_rate: usable_sample_rate(sample_rate),
            tone: [OnePoleTpt::new(); CHANNELS],
            tone_hz: f32::NAN,
            // One period due, so the very first sample is captured rather than
            // a zero being held over it.
            hold_phase: 1.0,
            held: [0.0; CHANNELS],
        }
    }

    fn reset(&mut self) {
        for filter in &mut self.tone {
            filter.reset();
        }
        self.hold_phase = 1.0;
        self.held = [0.0; CHANNELS];
    }

    /// Whether this block can be skipped entirely.
    ///
    /// Every knob at the position that means "off", with nothing ramping towards
    /// one that does not: the buffer keeps the exact bits it arrived with.
    fn is_transparent(&self) -> bool {
        let settled = |index: usize| !self.params.is_smoothing(index);
        settled(self.index.bits)
            && self.params.value(self.index.bits) >= MAX_BITS
            && settled(self.index.downsample)
            && self.params.value(self.index.downsample) <= 1.0
            && settled(self.index.tone)
            && self.params.value(self.index.tone) >= MAX_TONE_HZ
            && settled(self.index.mix)
            && self.params.value(self.index.mix) >= 100.0
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        let rate = self.sample_rate;
        let [left, right] = ctx.buffers.as_mut_slices();

        for frame in 0..frames {
            let bits = self.params.next(self.index.bits);
            let divisor = self.params.next(self.index.downsample).max(1.0);
            let tone = self.params.next(self.index.tone);
            let mix = self.params.next(self.index.mix) * 0.01;

            // One `tan` when the knob moves, none while it sits still.
            if tone != self.tone_hz {
                self.tone_hz = tone;
                let coefficient = OnePoleTpt::coefficient_for(tone, rate);
                for filter in &mut self.tone {
                    filter.set_coefficient(coefficient);
                }
            }

            let dry_l = left[frame];
            let dry_r = right[frame];

            self.hold_phase += 1.0 / divisor;
            if self.hold_phase >= 1.0 {
                self.hold_phase -= 1.0;
                self.held[0] = quantise(dry_l, bits);
                self.held[1] = quantise(dry_r, bits);
            }

            let filtered = if tone >= MAX_TONE_HZ {
                self.held
            } else {
                [
                    self.tone[0].low(self.held[0]),
                    self.tone[1].low(self.held[1]),
                ]
            };

            left[frame] = dry_l * (1.0 - mix) + filtered[0] * mix;
            right[frame] = dry_r * (1.0 - mix) + filtered[1] * mix;
        }
    }
}

impl RtNode for BitcrusherNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        self.params.begin_block();
        let bypassed = self.params.choice(self.index.bypass) != 0;
        let state = self.bypass.begin(ctx, bypassed);

        if state.processes() {
            if state.resets() {
                self.reset();
            }
            if self.is_transparent() {
                self.params.skip_all(ctx.frames);
            } else {
                self.render(ctx);
            }
        } else {
            self.params.skip_all(ctx.frames);
        }

        self.bypass.blend(ctx, state, bypassed);
    }
}

/// Builds [`BitcrusherNode`]s.
#[derive(Debug, Default)]
pub struct BitcrusherFactory;

impl BitcrusherFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for BitcrusherFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for BitcrusherFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(BitcrusherNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{level_db_at, render_mono, render_stereo, sine, Harness};

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&BitcrusherFactory::new(), SR, 512)
    }

    #[test]
    fn a_fresh_bitcrusher_is_bit_transparent() {
        let mut h = harness();
        let input: Vec<[f32; 2]> = (0..1_024)
            .map(|n| {
                let x = (n as f32 * 0.041).sin() * 0.6;
                [x, x * -0.4 + 0.05]
            })
            .collect();
        assert_eq!(render_stereo(&mut h, &input), input);
    }

    #[test]
    fn one_bit_really_is_three_levels_and_nothing_between_them() {
        // The claim the effect is named for: at the bottom of the knob every
        // sample lands on the mid-tread grid {-1, 0, +1}.
        let mut h = harness();
        h.set("bits", 1.0);
        render_mono(&mut h, &sine(220.0, 4_096, SR, 0.8)); // let the ramp settle
        let out = render_mono(&mut h, &sine(220.0, 4_096, SR, 0.8));
        for sample in &out {
            let nearest = sample.round();
            assert!(
                (sample - nearest).abs() < 1.0e-5 && nearest.abs() <= 1.0,
                "{sample} is not on the one-bit grid"
            );
        }
        // ...and it is not silence: a 0.8 sine crosses 0.5, so both outer levels
        // are used.
        assert!(out.iter().any(|s| *s >= 0.99));
        assert!(out.iter().any(|s| *s <= -0.99));
    }

    #[test]
    fn more_bits_means_a_smaller_error_and_the_error_is_the_quantiser_step() {
        let error_at = |bits: f32| {
            let mut h = harness();
            h.set("bits", bits);
            let signal = sine(220.0, 8_192, SR, 0.5);
            render_mono(&mut h, &signal);
            let out = render_mono(&mut h, &signal);
            out.iter()
                .zip(&signal)
                .map(|(y, x)| (y - x).abs())
                .fold(0.0_f32, f32::max)
        };
        for bits in [4.0_f32, 8.0, 12.0] {
            let step = 2.0_f32.powf(1.0 - bits);
            let worst = error_at(bits);
            // A mid-tread quantiser's worst error is half a step, whatever the
            // signal is.
            assert!(
                worst <= step * 0.5 + 1.0e-6,
                "{bits} bits: error {worst} exceeds half a step of {step}"
            );
            assert!(worst > step * 0.2, "{bits} bits: nothing was quantised");
        }
    }

    #[test]
    fn downsampling_holds_each_value_for_exactly_the_number_of_samples_it_says() {
        let mut h = harness();
        h.set("downsample", 8.0);
        h.set("bits", MAX_BITS);
        render_mono(&mut h, &sine(300.0, 4_096, SR, 0.7));
        let out = render_mono(&mut h, &sine(300.0, 4_096, SR, 0.7));

        // Count the runs of equal samples in the settled middle of the capture.
        let window = &out[1_024..3_072];
        let mut runs = Vec::new();
        let mut run = 1_usize;
        for pair in window.windows(2) {
            if pair[0] == pair[1] {
                run += 1;
            } else {
                runs.push(run);
                run = 1;
            }
        }
        assert!(!runs.is_empty(), "nothing was held at all");
        // Every run is one hold period. (The first and last are clipped by the
        // window, so they are dropped.)
        for length in &runs[1..runs.len() - 1] {
            assert_eq!(*length, 8, "a hold lasted {length} samples, not 8");
        }
    }

    #[test]
    fn the_zero_order_hold_puts_an_image_above_the_hold_rate() {
        // The metallic ring is the point: holding at fs/8 = 6 kHz mirrors a
        // 1 kHz tone to 5 kHz and 7 kHz, and those images are *not* filtered
        // away by anything.
        let mut h = harness();
        h.set("downsample", 8.0);
        let signal = sine(1_000.0, 24_000, SR, 0.7);
        render_mono(&mut h, &signal);
        let out = render_mono(&mut h, &signal);
        let window = &out[8_000..24_000];
        let fundamental = level_db_at(window, 1_000.0, SR);
        let image = level_db_at(window, 5_000.0, SR);
        assert!(
            image > fundamental - 22.0,
            "the 5 kHz image is {image:.1} dB against a {fundamental:.1} dB fundamental; \
             a decimator that quiet is really a lowpass"
        );
        // A clean signal has nothing there at all.
        let clean = level_db_at(&signal[8_000..24_000], 5_000.0, SR);
        assert!(image > clean + 30.0, "{image:.1} dB against {clean:.1} dB");
    }

    #[test]
    fn the_lofi_filter_takes_the_top_off_and_is_off_at_the_top_of_its_range() {
        let mut h = harness();
        h.set("bits", 8.0);
        h.set("tone", 1_000.0);
        let signal: Vec<f32> = (0..24_000)
            .map(|n| {
                let t = n as f64 / SR;
                0.4 * ((std::f64::consts::TAU * 300.0 * t).sin()
                    + (std::f64::consts::TAU * 8_000.0 * t).sin()) as f32
            })
            .collect();
        render_mono(&mut h, &signal);
        let out = render_mono(&mut h, &signal);
        let low = level_db_at(&out[8_000..], 300.0, SR);
        let high = level_db_at(&out[8_000..], 8_000.0, SR);
        assert!(
            high < low - 12.0,
            "a 1 kHz one-pole should be ~18 dB down at 8 kHz: {high:.1} against {low:.1}"
        );

        // At the top of the knob the filter is out of the path entirely.
        let mut open = harness();
        open.set("bits", 8.0);
        open.set("tone", MAX_TONE_HZ);
        render_mono(&mut open, &signal);
        let out = render_mono(&mut open, &signal);
        let high = level_db_at(&out[8_000..], 8_000.0, SR);
        let clean = level_db_at(&signal[8_000..], 8_000.0, SR);
        assert!(
            (high - clean).abs() < 1.0,
            "the filter is still in the path at the top of its range: {high:.1} vs {clean:.1}"
        );
    }

    #[test]
    fn mix_blends_against_the_untouched_input() {
        let mut h = harness();
        h.set("bits", 1.0);
        h.set(MIX_PARAM, 0.0);
        let input = vec![[0.3_f32, 0.3]; 4_096];
        let out = render_stereo(&mut h, &input);
        assert!(
            (out[3_000][0] - 0.3).abs() < 1.0e-5,
            "at 0 % mix the crushed path must be inaudible, got {}",
            out[3_000][0]
        );

        h.set(MIX_PARAM, 100.0);
        let out = render_stereo(&mut h, &input);
        // 0.3 quantised to the one-bit grid is 0.0.
        assert!(out[3_000][0].abs() < 1.0e-5, "{}", out[3_000][0]);
    }

    #[test]
    fn a_bypassed_bitcrusher_is_the_input_again() {
        let mut h = harness();
        h.set("bits", 2.0);
        h.set("downsample", 16.0);
        let input: Vec<[f32; 2]> = (0..4_096)
            .map(|n| [(n as f32 * 0.03).sin() * 0.5, (n as f32 * 0.07).sin() * 0.5])
            .collect();
        h.set("bypass", 1.0);
        render_stereo(&mut h, &input);
        let out = render_stereo(&mut h, &input);
        assert_eq!(out, input, "a settled bypass is the input, bit for bit");
    }
}
