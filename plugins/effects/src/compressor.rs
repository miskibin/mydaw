//! `effects.compressor` -- a feed-forward, log-domain compressor.
//!
//! ```text
//!            ┌──────────── detector ────────────┐
//!   in ──┬──▶│ SC HPF ▸ peak|RMS ▸ dB ▸ knee/ratio ▸ ballistics │──┐
//!        │   └──────────────────────────────────┘                  │ gain (dB)
//!        └────────────────────────── delay-free ───────────────────▼
//!                                              × ▸ makeup ▸ mix ▸ out
//! ```
//!
//! # Topology, and the paper that chose it
//! Giannoulis, Massberg & Reiss, *"Digital Dynamic Range Compressor Design -- A
//! Tutorial and Analysis"* (JAES 60(6), 2012):
//!
//! > the authors recommend **feed-forward** algorithms because they are stable
//! > and predictable. The **detector is placed in the log domain after the gain
//! > computer** since this generates a smooth envelope, no attack lag, and a
//! > variable knee width.
//!
//! That is exactly this: the gain computer runs first, on the level in decibels,
//! and the smoothing runs on its output. The specific detector is the paper's
//! *smooth decoupled peak detector*, which is the one that gives a release
//! curve without the discontinuity a plain decoupled detector has.
//!
//! Doing the ballistics **in dB** rather than on a linear gain is also the named
//! fix for the fast-attack distortion users report, so it is not only the
//! paper's recommendation.
//!
//! # The time-constant convention, and why it matters more than it looks
//! Attack and release here are **10 % → 90 %** times, so the coefficient is
//! `exp(-ln 9 / (τ·fs))`. There is no industry standard -- "after decades of
//! designing sundry dynamics processors there is not a single common standard"
//! -- but 10→90 is the mainstream DSP-tutorial convention, and the naive
//! `exp(-1/(τ·fs))` form is the 63 % one, which is **2.2× faster at the same
//! label**. A user A/B-ing this against Pro-C at "10 ms" would conclude it
//! "sounds different at the same settings", and they would be right.
//!
//! # Gain reduction is published, and the *maximum* is published too
//! Users report meters that miss real compression -- "sometimes the screen never
//! displays any change at all" -- because a whole attack and recovery can fall
//! between two UI frames. The fix developers name is to feed every sample into
//! the display and take the maximum within the buffer. So the node publishes two
//! readouts: the instantaneous reduction at the end of the block, and the
//! **largest** reduction anywhere in it. A meter should draw the second.
//!
//! # Auto-makeup is off by default, and that is a decision
//! "I find auto makeup gain flat out obnoxious and a guaranteed way to screw up
//! a mix." What users say they want instead is a level-matched bypass, which is
//! a shell feature and is specified in the research document. The parameter is
//! here because the other half of the forum wants it; the default is off because
//! that half is louder.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, ReadoutDef, RtNode,
    RtNodeFactory, SmoothedParams,
};

use crate::biquad::{highpass, Biquad, BiquadCoeffs};
use crate::dsp::{db_to_linear, usable_sample_rate};
use crate::effect::{index_of, Bypass, CachedDb, BYPASS_PARAM, MIX_PARAM};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.compressor";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "Compressor";

/// One line for a tooltip.
pub const DESCRIPTION: &str =
    "Feed-forward compressor with peak/RMS detection, soft knee and a sidechain highpass";

/// Level below which the detector reports silence, in dB. -120 dBFS.
const SILENCE_DB: f32 = -120.0;

/// Fastest attack the knob offers, in milliseconds.
///
/// Below 0.1 ms deliberately: the 1176 is 20 µs and The Glue starts at 0.01 ms,
/// and a compressor whose attack cannot reach transient-shaping territory is
/// missing a use case rather than a decimal place.
pub const MIN_ATTACK_MS: f32 = 0.05;

/// What the compressor exposes.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::new(
        "threshold",
        "Threshold",
        "dB",
        -60.0,
        0.0,
        -18.0,
        ParamCurve::Db,
    ),
    // Log, because 1:1 -> 2:1 is the same gesture as 8:1 -> 16:1. Used
    // internally as the slope `1 - 1/R`, which is the numerically stable form.
    ParamDef::new("ratio", "Ratio", ":1", 1.0, 20.0, 4.0, ParamCurve::Log),
    ParamDef::new(
        "attack",
        "Attack",
        "ms",
        MIN_ATTACK_MS,
        100.0,
        10.0,
        ParamCurve::Log,
    ),
    ParamDef::new(
        "release",
        "Release",
        "ms",
        5.0,
        2_000.0,
        100.0,
        ParamCurve::Log,
    ),
    ParamDef::toggle("auto_release", "Auto Release", false),
    ParamDef::new("knee", "Knee", "dB", 0.0, 18.0, 6.0, ParamCurve::Linear),
    // Peak for drums and transients, RMS for vocals and levelling -- the split
    // the forums describe consistently.
    ParamDef::choice("detector", "Detector", 2, 0),
    ParamDef::new(
        "rms_ms",
        "RMS Window",
        "ms",
        1.0,
        100.0,
        10.0,
        ParamCurve::Log,
    ),
    ParamDef::toggle("sc_hpf_on", "SC Filter", false),
    ParamDef::new(
        "sc_hpf",
        "SC Freq",
        "Hz",
        20.0,
        500.0,
        100.0,
        ParamCurve::Log,
    ),
    ParamDef::new("makeup", "Makeup", "dB", -24.0, 24.0, 0.0, ParamCurve::Db),
    ParamDef::toggle("auto_makeup", "Auto Makeup", false),
    ParamDef::new(MIX_PARAM, "Mix", "%", 0.0, 100.0, 100.0, ParamCurve::Linear),
];

/// What the compressor publishes back.
pub static READOUTS: &[ReadoutDef] = &[
    ReadoutDef::new("gr", "Gain Reduction", "dB"),
    ReadoutDef::new("gr_max", "Max Gain Reduction", "dB"),
];

/// Index of the instantaneous gain-reduction readout.
pub const READOUT_GR: usize = 0;
/// Index of the per-block maximum gain-reduction readout.
pub const READOUT_GR_MAX: usize = 1;

/// The one-pole coefficient for a **10 % to 90 %** time of `ms`.
///
/// `ln 9 ≈ 2.1972` is the span from 10 % to 90 % of an exponential. See the
/// module docs on why this and not the 63 % form.
pub fn ballistic_coefficient(ms: f32, sample_rate: f32) -> f32 {
    if !ms.is_finite() || ms <= 0.0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
        return 0.0;
    }
    (-2.197_224_6 / (ms * 0.001 * sample_rate)).exp()
}

/// The static gain-reduction curve, in dB.
///
/// `level_db` in, **positive** reduction out. Public so the future transfer-curve
/// display and `tests/compressor_dsp.rs` read the same function the audio path
/// does -- the same argument as the EQ's [`magnitude_db_at`](crate::eq::magnitude_db_at).
pub fn reduction_db(level_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    // Slope, not ratio: `S = 1 - 1/R` is the standard formulation, it is what
    // the arithmetic below actually wants, and it stays finite as R grows.
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    let over = level_db - threshold_db;
    if knee_db > 0.0 && 2.0 * over.abs() <= knee_db {
        // Inside the knee: a quadratic that is tangent to both straight parts,
        // so the curve has no corner and neither does its first derivative.
        let x = over + knee_db * 0.5;
        slope * x * x / (2.0 * knee_db)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

/// The makeup the auto-makeup switch applies, in dB.
///
/// Signal-independent by construction -- "a trivial equation involving the
/// threshold and the ratio" -- and specifically **not** a measurement of the
/// live signal, which is the implementation every complaint about auto-gain is
/// actually describing. It is the reduction the compressor applies to a
/// full-scale peak, given back: a signal that used to touch 0 dBFS still does.
pub fn auto_makeup_db(threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    reduction_db(0.0, threshold_db, ratio, knee_db)
}

/// Parameter indices, resolved once off the audio thread.
#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    threshold: usize,
    ratio: usize,
    attack: usize,
    release: usize,
    auto_release: usize,
    knee: usize,
    detector: usize,
    rms_ms: usize,
    sc_hpf_on: usize,
    sc_hpf: usize,
    makeup: usize,
    auto_makeup: usize,
    mix: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        Self {
            bypass: index_of(block, BYPASS_PARAM),
            threshold: index_of(block, "threshold"),
            ratio: index_of(block, "ratio"),
            attack: index_of(block, "attack"),
            release: index_of(block, "release"),
            auto_release: index_of(block, "auto_release"),
            knee: index_of(block, "knee"),
            detector: index_of(block, "detector"),
            rms_ms: index_of(block, "rms_ms"),
            sc_hpf_on: index_of(block, "sc_hpf_on"),
            sc_hpf: index_of(block, "sc_hpf"),
            makeup: index_of(block, "makeup"),
            auto_makeup: index_of(block, "auto_makeup"),
            mix: index_of(block, MIX_PARAM),
        }
    }
}

/// One branch of the smooth decoupled peak detector.
#[derive(Debug, Clone, Copy, Default)]
struct Branch {
    /// The release stage's memory.
    y1: f32,
    /// The attack stage's memory. This is the reduction actually applied.
    y: f32,
}

impl Branch {
    /// One sample of the paper's smooth decoupled detector, in dB.
    #[inline]
    fn next(&mut self, target: f32, attack: f32, release: f32) -> f32 {
        self.y1 = target.max(release * self.y1 + (1.0 - release) * target);
        self.y = attack * self.y + (1.0 - attack) * self.y1;
        if self.y.abs() < 1.0e-9 {
            self.y = 0.0;
        }
        self.y
    }

    fn reset(&mut self) {
        self.y1 = 0.0;
        self.y = 0.0;
    }
}

/// The compressor node.
pub struct CompressorNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    makeup: CachedDb,
    sample_rate: f32,
    /// Sidechain highpass, one section per channel.
    sc_coeffs: BiquadCoeffs,
    sc_state: [Biquad; 2],
    sc_hz: f32,
    /// Running mean square, for the RMS detector.
    mean_square: f32,
    /// The main ballistics branch.
    fast: Branch,
    /// The second, slower branch the auto-release option runs alongside it.
    slow: Branch,
}

impl CompressorNode {
    /// **Non-RT**: allocates.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let index = Indices::new(&params);
        let smoothed = SmoothedParams::new(params, sample_rate);
        let rate = usable_sample_rate(sample_rate);
        let mut node = Self {
            makeup: CachedDb::new(index.makeup, &smoothed),
            params: smoothed,
            index,
            bypass: Bypass::new(max_block, sample_rate),
            sample_rate: rate,
            sc_coeffs: BiquadCoeffs::BYPASS,
            sc_state: [Biquad::new(); 2],
            sc_hz: f32::NAN,
            mean_square: 0.0,
            fast: Branch::default(),
            slow: Branch::default(),
        };
        node.refresh_sidechain();
        node
    }

    fn reset(&mut self) {
        self.sc_state = [Biquad::new(); 2];
        self.mean_square = 0.0;
        self.fast.reset();
        self.slow.reset();
    }

    /// Recompute the sidechain filter, only when its frequency has changed.
    fn refresh_sidechain(&mut self) {
        let hz = self.params.value(self.index.sc_hpf);
        if hz == self.sc_hz {
            return;
        }
        self.sc_hz = hz;
        self.sc_coeffs = highpass(
            f64::from(hz),
            std::f64::consts::FRAC_1_SQRT_2,
            f64::from(self.sample_rate),
        );
    }

    #[allow(clippy::too_many_lines)]
    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        // Read once per block: switches never ramp, and the ballistics
        // coefficients involve an `exp` that is not worth paying per sample.
        let rms_mode = self.params.choice(self.index.detector) == 1;
        let auto_release = self.params.choice(self.index.auto_release) != 0;
        let sc_on = self.params.choice(self.index.sc_hpf_on) != 0;
        let auto_makeup = self.params.choice(self.index.auto_makeup) != 0;
        self.refresh_sidechain();

        let attack = ballistic_coefficient(self.params.value(self.index.attack), self.sample_rate);
        let release_ms = self.params.value(self.index.release);
        let release = ballistic_coefficient(release_ms, self.sample_rate);
        // The Glue's scheme: "a slow one as a base compression value, and a fast
        // one to react to transients". The branch producing the *most* reduction
        // takes control, so a transient recovers on the fast curve while a
        // sustained passage keeps the slow one's hold.
        let release_slow = ballistic_coefficient(release_ms * 4.0, self.sample_rate);
        let release_fast = ballistic_coefficient(release_ms * 0.25, self.sample_rate);
        // The RMS window as a one-pole on the mean square.
        let rms_coeff =
            ballistic_coefficient(self.params.value(self.index.rms_ms), self.sample_rate);

        let mut peak_reduction = 0.0_f32;
        let mut last_reduction = 0.0_f32;

        let channels = ctx.buffers.as_mut_slices();
        let [left, right] = channels;
        for frame in 0..frames {
            let threshold = self.params.next(self.index.threshold);
            let ratio = self.params.next(self.index.ratio);
            let knee = self.params.next(self.index.knee);
            let mix = self.params.next(self.index.mix) * 0.01;
            let makeup = self.makeup.next(&mut self.params);
            // Advance the parameters this block is not reading per sample, so
            // "settled" is a truthful answer next block.
            self.params.next(self.index.sc_hpf);
            self.params.next(self.index.rms_ms);
            self.params.next(self.index.attack);
            self.params.next(self.index.release);

            let dry_l = left[frame];
            let dry_r = right[frame];

            // --- detector ------------------------------------------------
            // Stereo-linked: one gain for both channels, so a hard-panned
            // transient cannot shift the image by ducking one side.
            let mut det_l = dry_l;
            let mut det_r = dry_r;
            if sc_on {
                det_l = self.sc_state[0].process(f64::from(det_l), &self.sc_coeffs) as f32;
                det_r = self.sc_state[1].process(f64::from(det_r), &self.sc_coeffs) as f32;
            }
            let level = if rms_mode {
                let instant = 0.5 * (det_l * det_l + det_r * det_r);
                self.mean_square = instant + rms_coeff * (self.mean_square - instant);
                if self.mean_square.abs() < 1.0e-20 {
                    self.mean_square = 0.0;
                }
                self.mean_square.sqrt()
            } else {
                det_l.abs().max(det_r.abs())
            };
            let level_db = if level > 1.0e-6 {
                20.0 * level.log10()
            } else {
                SILENCE_DB
            };

            // --- gain computer, then ballistics, both in dB ---------------
            let target = reduction_db(level_db, threshold, ratio, knee);
            let reduction = if auto_release {
                let a = self.fast.next(target, attack, release_fast);
                let b = self.slow.next(target, attack, release_slow);
                a.max(b)
            } else {
                self.fast.next(target, attack, release)
            };
            peak_reduction = peak_reduction.max(reduction);
            last_reduction = reduction;

            // --- apply ----------------------------------------------------
            let auto = if auto_makeup {
                auto_makeup_db(threshold, ratio, knee)
            } else {
                0.0
            };
            let gain = db_to_linear(auto - reduction) * makeup;
            left[frame] = dry_l * (1.0 - mix) + dry_l * gain * mix;
            right[frame] = dry_r * (1.0 - mix) + dry_r * gain * mix;
        }

        let block = self.params.block();
        block.set_readout(READOUT_GR, last_reduction);
        block.set_readout(READOUT_GR_MAX, peak_reduction);
    }
}

impl RtNode for CompressorNode {
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
            // A bypassed compressor is not compressing, and its meter must say
            // so rather than freezing at whatever it last showed.
            let block = self.params.block();
            block.set_readout(READOUT_GR, 0.0);
            block.set_readout(READOUT_GR_MAX, 0.0);
        }

        self.bypass.blend(ctx, state, bypassed);
    }
}

/// Builds [`CompressorNode`]s.
#[derive(Debug, Default)]
pub struct CompressorFactory;

impl CompressorFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for CompressorFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for CompressorFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn readouts(&self) -> &'static [ReadoutDef] {
        READOUTS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(CompressorNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        db, peak_mono, render_mono, render_stereo, rms_mono, silence, sine, Harness,
    };

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&CompressorFactory::new(), SR, 512)
    }

    /// Steady-state output level for a steady sine at `in_db`.
    fn static_response_db(h: &mut Harness, in_db: f32) -> f32 {
        let amplitude = db_to_linear(in_db);
        let input = sine(1_000.0, 48_000, SR, amplitude);
        let out = render_mono(h, &input);
        // Well past attack, release and the parameter ramps.
        db(peak_mono(&out[36_000..]))
    }

    #[test]
    fn a_fresh_compressor_is_bit_transparent() {
        // Threshold -18 dB, but nothing quiet is touched, and the default has to
        // be a genuine pass-through for a signal below it.
        let mut h = harness();
        let input: Vec<[f32; 2]> = (0..2_048)
            .map(|n| {
                let x = (n as f32 * 0.03).sin() * 0.05;
                [x, -x]
            })
            .collect();
        assert_eq!(render_stereo(&mut h, &input), input);
    }

    #[test]
    fn the_static_curve_is_the_threshold_and_ratio_arithmetic() {
        let mut h = harness();
        h.set("knee", 0.0);
        h.set("ratio", 4.0);
        h.set("threshold", -20.0);
        h.set("attack", 1.0);
        h.set("release", 50.0);

        // Below the threshold: untouched.
        let quiet = static_response_db(&mut h, -40.0);
        assert!((quiet + 40.0).abs() < 0.3, "{quiet}");

        // Above it: 4:1 means three quarters of the excess disappears.
        for input_db in [-12.0_f32, -6.0, 0.0] {
            let expected = -20.0 + (input_db + 20.0) / 4.0;
            let measured = static_response_db(&mut h, input_db);
            assert!(
                (measured - expected).abs() < 0.5,
                "{input_db} dB in: expected {expected} dB, measured {measured} dB"
            );
        }
    }

    #[test]
    fn the_soft_knee_rounds_the_corner_it_says_it_does() {
        // At the threshold exactly, a hard knee does nothing and a knee of `W`
        // is already `slope * W / 8` into the reduction.
        let hard = reduction_db(-20.0, -20.0, 4.0, 0.0);
        assert_eq!(hard, 0.0);
        let soft = reduction_db(-20.0, -20.0, 4.0, 12.0);
        let slope = 1.0 - 1.0 / 4.0;
        assert!((soft - slope * 12.0 / 8.0).abs() < 1e-5, "{soft}");

        // Continuous, and with a continuous derivative, across the whole knee:
        // that is what "soft" has to mean or it is just a different corner.
        let mut previous = reduction_db(-40.0, -20.0, 4.0, 12.0);
        let mut last_step = 0.0_f32;
        for step in -400..=0 {
            let level = step as f32 / 10.0;
            let value = reduction_db(level, -20.0, 4.0, 12.0);
            let delta = value - previous;
            assert!(delta >= -1e-6, "the curve must be monotone at {level}");
            assert!(
                (delta - last_step).abs() < 0.02,
                "the derivative jumps at {level}"
            );
            previous = value;
            last_step = delta;
        }
    }

    #[test]
    fn the_ratio_reaches_the_slope_it_advertises() {
        for ratio in [2.0_f32, 4.0, 10.0, 20.0] {
            let slope = reduction_db(0.0, -20.0, ratio, 0.0) / 20.0;
            assert!(
                (slope - (1.0 - 1.0 / ratio)).abs() < 1e-5,
                "{ratio}:1 gave slope {slope}"
            );
        }
        assert_eq!(reduction_db(0.0, -20.0, 1.0, 0.0), 0.0, "1:1 is a wire");
    }

    #[test]
    fn attack_and_release_take_the_time_they_claim() {
        // 10 % -> 90 %, which is the convention this compressor documents.
        let coefficient = ballistic_coefficient(10.0, SR as f32);
        let frames = (0.010 * SR) as usize;
        // Start at 10 %, which is where the convention's clock starts.
        let (mut y, target) = (0.1_f32, 1.0_f32);
        for _ in 0..frames {
            y = coefficient * y + (1.0 - coefficient) * target;
        }
        assert!(
            (y - 0.9).abs() < 0.005,
            "10 ms should carry 10 % to 90 %, got {y}"
        );

        // And the 63 % convention would be materially different, which is the
        // whole reason for documenting which one this is.
        let sixty_three = (-1.0_f32 / (0.010 * SR as f32)).exp();
        assert!(
            (sixty_three - coefficient).abs() > 1e-4,
            "the two conventions must not be the same number"
        );
    }

    #[test]
    fn gain_reduction_is_published_and_the_block_maximum_is_the_one_a_meter_wants() {
        let mut h = harness();
        h.set("threshold", -30.0);
        h.set("ratio", 8.0);
        h.set("attack", 0.5);
        h.set("release", 200.0);

        // A short burst inside one block, then silence: the instantaneous value
        // decays away while the block maximum caught it.
        let mut input = sine(1_000.0, 4_096, SR, 0.9);
        input.extend(silence(4_096));
        render_mono(&mut h, &input);
        assert!(
            h.readout("gr_max") > 10.0,
            "8:1 from -30 dB on a full-scale sine should be plenty: {}",
            h.readout("gr_max")
        );
        assert!(h.readout("gr_max") >= h.readout("gr"));

        // Silence for long enough and the meter comes back to zero.
        render_mono(&mut h, &silence(48_000));
        assert!(h.readout("gr") < 0.5, "{}", h.readout("gr"));
    }

    #[test]
    fn a_bypassed_compressor_reports_no_reduction_at_all() {
        let mut h = harness();
        h.set("threshold", -40.0);
        h.set("ratio", 10.0);
        render_mono(&mut h, &sine(1_000.0, 8_192, SR, 0.8));
        assert!(h.readout("gr") > 1.0);

        h.set("bypass", 1.0);
        render_mono(&mut h, &sine(1_000.0, 8_192, SR, 0.8));
        render_mono(&mut h, &sine(1_000.0, 8_192, SR, 0.8));
        assert_eq!(h.readout("gr"), 0.0, "a bypassed meter must not freeze");
        assert_eq!(h.readout("gr_max"), 0.0);
    }

    #[test]
    fn compression_reduces_the_crest_factor_it_is_asked_to() {
        // The claim a compressor exists to make, measured rather than asserted:
        // peak-to-RMS goes down.
        // A drum-like pulse train: a one-millisecond rise and a fast decay,
        // repeating every tenth of a second. The rise matters -- a pulse whose
        // peak is its *first sample* cannot be caught by any compressor without
        // lookahead, and testing against one would be testing the wrong thing.
        let signal: Vec<f32> = (0..48_000)
            .map(|n| {
                let phase = n % 4_800;
                let attack = (phase as f32 / 48.0).min(1.0);
                let decay = (-(phase as f32) / 480.0).exp();
                attack * decay * (n as f32 * 0.31).sin() * 0.9
            })
            .collect();

        let mut h = harness();
        h.set("bypass", 1.0);
        let dry = render_mono(&mut h, &signal);
        let dry_crest = db(peak_mono(&dry[8_000..]) / rms_mono(&dry[8_000..]));

        let mut h = harness();
        h.set("threshold", -36.0);
        h.set("ratio", 12.0);
        // Fast enough to catch the transient itself. With a slow attack the
        // peaks escape and only the decay is compressed, which *raises* the
        // crest factor -- correct behaviour, and a different measurement.
        h.set("attack", MIN_ATTACK_MS);
        // Shorter than the 100 ms between hits, so the gain recovers in the
        // gaps. A release longer than the pattern scales everything by the same
        // amount and leaves the crest factor exactly where it was -- correct
        // behaviour, and not what this measurement is about.
        h.set("release", 30.0);
        let wet = render_mono(&mut h, &signal);
        let wet_crest = db(peak_mono(&wet[8_000..]) / rms_mono(&wet[8_000..]));

        println!("crest factor: {dry_crest:.2} dB dry, {wet_crest:.2} dB compressed");
        assert!(
            wet_crest < dry_crest - 3.0,
            "crest factor only went from {dry_crest:.2} dB to {wet_crest:.2} dB"
        );
    }

    #[test]
    fn the_rms_detector_ignores_a_transient_the_peak_detector_catches() {
        // The reason the switch exists, as a measurement.
        let mut signal = silence(2_400);
        signal.extend(sine(1_000.0, 48, SR, 0.95)); // a 1 ms spike
        signal.extend(silence(9_600));

        let reduction = |rms: bool| {
            let mut h = harness();
            h.set("threshold", -30.0);
            h.set("ratio", 10.0);
            h.set("attack", 0.05);
            h.set("release", 300.0);
            h.set("detector", if rms { 1.0 } else { 0.0 });
            h.set("rms_ms", 100.0);
            render_mono(&mut h, &signal);
            h.readout("gr_max")
        };

        let peak = reduction(false);
        let rms = reduction(true);
        println!("a 1 ms spike: peak detector {peak:.2} dB, 100 ms RMS {rms:.2} dB");
        assert!(
            peak > rms + 3.0,
            "peak caught {peak:.2} dB and a 100 ms RMS window caught {rms:.2} dB; \
             the switch is not doing anything"
        );
    }

    #[test]
    fn the_sidechain_filter_stops_the_bass_pumping_everything_else() {
        // A loud 40 Hz tone under a quiet 3 kHz one. With the filter off the
        // bass drives the detector; with it on at 200 Hz it should not.
        let signal: Vec<f32> = (0..48_000)
            .map(|n| {
                let t = n as f64 / SR;
                (0.8 * (std::f64::consts::TAU * 40.0 * t).sin()
                    + 0.05 * (std::f64::consts::TAU * 3_000.0 * t).sin()) as f32
            })
            .collect();

        let reduction = |filtered: bool| {
            let mut h = harness();
            h.set("threshold", -24.0);
            h.set("ratio", 8.0);
            h.set("attack", 5.0);
            h.set("release", 100.0);
            h.set("sc_hpf_on", if filtered { 1.0 } else { 0.0 });
            h.set("sc_hpf", 200.0);
            render_mono(&mut h, &signal);
            h.readout("gr_max")
        };

        let open = reduction(false);
        let filtered = reduction(true);
        assert!(
            filtered < open - 6.0,
            "the sidechain filter only took {open:.1} dB down to {filtered:.1} dB"
        );
    }

    #[test]
    fn auto_makeup_is_a_function_of_the_knobs_and_nothing_else() {
        // Signal-independent by construction: the same settings give the same
        // number whatever is playing, which is the whole point.
        assert_eq!(auto_makeup_db(-20.0, 4.0, 0.0), 15.0, "3/4 of 20 dB");
        assert_eq!(auto_makeup_db(-20.0, 1.0, 0.0), 0.0, "1:1 needs none");
        assert!(auto_makeup_db(0.0, 20.0, 0.0).abs() < 1e-6);

        // And it is off by default. See the module docs on why.
        let h = harness();
        assert_eq!(h.get("auto_makeup"), 0.0);
    }

    #[test]
    fn the_mix_knob_blends_towards_the_untouched_signal() {
        let mut h = harness();
        h.set("threshold", -40.0);
        h.set("ratio", 20.0);
        h.set("attack", 1.0);
        h.set("release", 60.0);
        let input = sine(1_000.0, 48_000, SR, 0.8);

        let full = rms_mono(&render_mono(&mut h, &input)[36_000..]);
        h.set(MIX_PARAM, 0.0);
        let none = rms_mono(&render_mono(&mut h, &input)[36_000..]);
        assert!(
            (db(none) - db(rms_mono(&input))).abs() < 0.1,
            "mix at 0 % must be the dry signal"
        );
        assert!(db(full) < db(none) - 6.0, "and 100 % must be doing work");
    }

    #[test]
    fn auto_release_holds_longer_on_sustained_material_than_on_a_transient() {
        // The Glue's claim, measured: two branches, the slower one keeping a
        // base level of compression under a sustained passage.
        let recovery = |auto: bool| {
            let mut h = harness();
            h.set("threshold", -30.0);
            h.set("ratio", 8.0);
            h.set("attack", 1.0);
            h.set("release", 100.0);
            h.set("auto_release", if auto { 1.0 } else { 0.0 });
            // A long loud passage, then a gap.
            let mut signal = sine(1_000.0, 24_000, SR, 0.9);
            signal.extend(silence(4_800)); // 100 ms of gap
            render_mono(&mut h, &signal);
            h.readout("gr")
        };
        assert!(
            recovery(true) > recovery(false) + 0.5,
            "auto release recovered to {:.2} dB and fixed release to {:.2} dB",
            recovery(true),
            recovery(false)
        );
    }
}
