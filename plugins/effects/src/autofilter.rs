//! `effects.autofilter` -- a resonant state-variable filter with an LFO and an
//! envelope follower on its cutoff.
//!
//! ```text
//!                    ┌── LFO (free Hz or tempo-synced) ──┐
//!   in ─┬──▶ SVF (LP | HP | BP) ◀── cutoff · 2^(lfo + env) ┴── envelope of in
//!       │        │
//!       └── dry ──┴──▶ mix ─▶ out
//! ```
//!
//! # Why the modulation is in octaves and not in hertz
//! A filter sweep is heard as a musical interval, so a modulation depth measured
//! in hertz would be a huge sweep at the bottom of the knob and an inaudible one
//! at the top. The LFO and the envelope both scale the cutoff by `2^x`, which
//! makes "depth" mean the same thing wherever the cutoff is parked, and makes
//! the shaded band an editor can draw around the curve a constant width on a
//! logarithmic axis.
//!
//! # The envelope amount is bipolar on purpose
//! Positive amount opens the filter as the signal gets louder -- the auto-wah
//! everybody knows. Negative amount *closes* it, which is the less common but
//! far more useful setting on a busy bus: the loud passages get darker and the
//! quiet ones keep their air, which is a ducking tone control that no
//! compressor can do.
//!
//! # Resonance is the same shape in every mode
//! The bandpass output of a state-variable filter has a peak gain of `Q`, so an
//! honest resonance knob would double as a volume knob in one of the three
//! modes. It is normalised by `1/Q` here (see [`Svf::mode`]), so turning
//! resonance up changes the *shape* in all three and the level only where the
//! resonance actually is.
//!
//! # Wide open is a real pass-through
//! At its defaults -- lowpass, cutoff at the top, no resonance, no modulation --
//! the filter is switched out of the path entirely rather than run at 20 kHz, so
//! a freshly inserted auto-filter is bit-transparent like everything else in the
//! pack. Move any of those five and the filter is in the path from that sample.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, ReadoutDef, RtNode,
    RtNodeFactory, SmoothedParams, CHANNELS,
};

use crate::delay::DIVISION_BEATS;
use crate::dsp::{usable_sample_rate, EnvelopeFollower, Lfo, Svf};
use crate::effect::{index_of, Bypass, BYPASS_PARAM, MIX_PARAM};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.autofilter";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "Auto Filter";

/// One line for a tooltip.
pub const DESCRIPTION: &str =
    "Resonant lowpass, highpass or bandpass swept by a synced LFO and an envelope follower";

/// Bottom of the cutoff knob, in Hz.
pub const MIN_CUTOFF_HZ: f32 = 30.0;

/// Top of the cutoff knob, and the position at which the filter is switched out.
pub const MAX_CUTOFF_HZ: f32 = 20_000.0;

/// Resonance at the bottom of the knob: a Butterworth-ish, peak-free corner.
pub const MIN_Q: f32 = 0.7;

/// Resonance at the top of the knob. Sharp, and short of self-oscillation.
pub const MAX_Q: f32 = 12.0;

/// How far a fully-turned LFO amount sweeps the cutoff, in octaves either way.
pub const MAX_LFO_OCTAVES: f32 = 3.0;

/// How far a fully-turned envelope amount moves the cutoff, in octaves.
pub const MAX_ENV_OCTAVES: f32 = 4.0;

/// Input level at which a fully-turned envelope amount reaches its full travel.
///
/// -6 dBFS. An envelope amount has to be measured against *something*, and this
/// is the same choice the delay's duck makes: normal programme material engages
/// the modulation fully, and the gaps let it back.
pub const ENV_REFERENCE: f32 = 0.5;

/// How long the cutoff knob takes to cross its whole range.
///
/// Longer than the standard 10 ms ramp. A cutoff is not a gain: sweeping a
/// resonant filter across ten octaves in ten milliseconds excites the filter's
/// own transient, which is a thump rather than the sweep the drag asked for.
/// 60 ms still feels immediate under a finger and leaves the modulation --
/// which is applied on top of this and not smoothed by it -- as fast as ever.
pub const SWEEP_MS: f32 = 60.0;

/// Samples between two re-aimings of the filter. See `phaser::CONTROL_INTERVAL`.
pub const CONTROL_INTERVAL: usize = 16;

/// Filter modes, in the order the switch offers them.
pub const MODE_LOWPASS: usize = 0;
pub const MODE_HIGHPASS: usize = 1;
pub const MODE_BANDPASS: usize = 2;

/// What the auto-filter exposes.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::choice("mode", "Mode", 3, 0),
    ParamDef::new(
        "cutoff",
        "Cutoff",
        "Hz",
        MIN_CUTOFF_HZ,
        MAX_CUTOFF_HZ,
        MAX_CUTOFF_HZ,
        ParamCurve::Log,
    )
    .smoothed(SWEEP_MS),
    ParamDef::new("res", "Resonance", "%", 0.0, 100.0, 0.0, ParamCurve::Linear),
    ParamDef::new(
        "lfo_amount",
        "LFO Amount",
        "%",
        0.0,
        100.0,
        0.0,
        ParamCurve::Linear,
    ),
    ParamDef::toggle("lfo_sync", "LFO Sync", false),
    ParamDef::new(
        "lfo_rate",
        "LFO Rate",
        "Hz",
        0.01,
        20.0,
        1.0,
        ParamCurve::Log,
    ),
    ParamDef::choice("lfo_div", "LFO Division", 7, 2),
    ParamDef::new("spread", "Spread", "%", 0.0, 100.0, 0.0, ParamCurve::Linear),
    ParamDef::new(
        "env_amount",
        "Env Amount",
        "%",
        -100.0,
        100.0,
        0.0,
        ParamCurve::Linear,
    ),
    ParamDef::new(
        "env_attack",
        "Env Attack",
        "ms",
        0.1,
        100.0,
        5.0,
        ParamCurve::Log,
    ),
    ParamDef::new(
        "env_release",
        "Env Release",
        "ms",
        10.0,
        1_000.0,
        120.0,
        ParamCurve::Log,
    ),
    ParamDef::new(MIX_PARAM, "Mix", "%", 0.0, 100.0, 100.0, ParamCurve::Linear),
];

/// What the auto-filter publishes back.
///
/// The cutoff *in force* -- after the LFO and the envelope have moved it -- and
/// the follower's own output. Neither can be derived from the parameter values:
/// one depends on where the LFO happens to be and the other on the audio, so an
/// editor that wants to draw the filter where it actually is has to be told.
pub static READOUTS: &[ReadoutDef] = &[
    ReadoutDef::new("cutoff", "Cutoff", "Hz"),
    ReadoutDef::new("env", "Envelope", ""),
];

/// Index of the modulated-cutoff readout.
pub const READOUT_CUTOFF: usize = 0;

/// Index of the envelope readout.
pub const READOUT_ENV: usize = 1;

/// Resonance percentage as the filter's Q.
///
/// Exponential between [`MIN_Q`] and [`MAX_Q`], because Q is a ratio: a linear
/// map would spend the bottom half of the knob on a difference nobody can hear
/// and the top half going from sharp to sharper.
pub fn resonance_q(percent: f32) -> f32 {
    let t = (percent * 0.01).clamp(0.0, 1.0);
    MIN_Q * (MAX_Q / MIN_Q).powf(t)
}

/// LFO rate for a sync division at `bpm`, in Hz.
///
/// One cycle per division: 1/4 at 120 BPM is two cycles a second. Public so the
/// editor can print the resolved rate from the same mapping the audio uses.
pub fn sync_rate_hz(division: usize, bpm: f64) -> f32 {
    let beats = DIVISION_BEATS[division.min(DIVISION_BEATS.len() - 1)];
    let bpm = if bpm.is_finite() && bpm > 0.0 {
        bpm
    } else {
        mydaw_rt_core::DEFAULT_BPM
    };
    (bpm / 60.0 / beats) as f32
}

/// Parameter indices, resolved once off the audio thread.
#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    mode: usize,
    cutoff: usize,
    res: usize,
    lfo_amount: usize,
    lfo_sync: usize,
    lfo_rate: usize,
    lfo_div: usize,
    spread: usize,
    env_amount: usize,
    env_attack: usize,
    env_release: usize,
    mix: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        Self {
            bypass: index_of(block, BYPASS_PARAM),
            mode: index_of(block, "mode"),
            cutoff: index_of(block, "cutoff"),
            res: index_of(block, "res"),
            lfo_amount: index_of(block, "lfo_amount"),
            lfo_sync: index_of(block, "lfo_sync"),
            lfo_rate: index_of(block, "lfo_rate"),
            lfo_div: index_of(block, "lfo_div"),
            spread: index_of(block, "spread"),
            env_amount: index_of(block, "env_amount"),
            env_attack: index_of(block, "env_attack"),
            env_release: index_of(block, "env_release"),
            mix: index_of(block, MIX_PARAM),
        }
    }
}

/// The auto-filter node.
pub struct AutoFilterNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    filters: [Svf; CHANNELS],
    envelope: EnvelopeFollower,
    lfo: Lfo,
    /// Last rate the LFO increment was computed for.
    rate_hz: f32,
    /// Last (attack, release) the follower's coefficients were computed for.
    env_times: (f32, f32),
    sample_rate: f32,
    control: usize,
    /// The cutoff the left channel is running at, for the readout.
    live_cutoff: f32,
}

impl AutoFilterNode {
    /// **Non-RT**: allocates the bypass tap.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let index = Indices::new(&params);
        let rate = usable_sample_rate(sample_rate);
        let smoothed = SmoothedParams::new(params, sample_rate);
        let cutoff = smoothed.value(index.cutoff);
        let q = resonance_q(smoothed.value(index.res));
        let mut lfo = Lfo::new();
        lfo.set_rate(smoothed.value(index.lfo_rate), rate);
        // The follower is armed here rather than on the first `render`, because
        // the transparency fast path keeps it warm and would otherwise run it
        // with the instantaneous coefficients a default-constructed one has.
        let attack = smoothed.value(index.env_attack);
        let release = smoothed.value(index.env_release);
        let mut envelope = EnvelopeFollower::new();
        envelope.set_times(attack, release, rate);
        Self {
            rate_hz: smoothed.value(index.lfo_rate),
            filters: [Svf::new(cutoff, q, rate); CHANNELS],
            envelope,
            lfo,
            env_times: (attack, release),
            sample_rate: rate,
            control: 0,
            live_cutoff: cutoff,
            params: smoothed,
            index,
            bypass: Bypass::new(max_block, sample_rate),
        }
    }

    fn reset(&mut self) {
        for filter in &mut self.filters {
            filter.reset();
        }
        self.envelope.reset();
        self.lfo.reset();
        self.control = 0;
    }

    /// Whether the filter is parked where it does nothing at all.
    ///
    /// Lowpass, cutoff at the top of the knob, no resonance, no modulation and a
    /// fully wet mix, with nothing ramping towards a setting that is not: the
    /// buffer keeps the exact bits it arrived with.
    fn is_transparent(&self) -> bool {
        let settled = |index: usize| !self.params.is_smoothing(index);
        self.params.choice(self.index.mode) == MODE_LOWPASS
            && settled(self.index.cutoff)
            && self.params.value(self.index.cutoff) >= MAX_CUTOFF_HZ
            && settled(self.index.res)
            && self.params.value(self.index.res) <= 0.0
            && settled(self.index.lfo_amount)
            && self.params.value(self.index.lfo_amount) <= 0.0
            && settled(self.index.env_amount)
            && self.params.value(self.index.env_amount) == 0.0
            && settled(self.index.mix)
            && self.params.value(self.index.mix) >= 100.0
    }

    /// The LFO rate this block runs at, resolving a sync division if one is on.
    fn lfo_rate(&self, bpm: f64) -> f32 {
        if self.params.choice(self.index.lfo_sync) != 0 {
            sync_rate_hz(self.params.choice(self.index.lfo_div), bpm)
        } else {
            self.params.goal(self.index.lfo_rate)
        }
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        let mode = self.params.choice(self.index.mode).min(MODE_BANDPASS);
        let target_rate = self.lfo_rate(ctx.transport.bpm);
        if target_rate != self.rate_hz {
            self.rate_hz = target_rate;
            self.lfo.set_rate(target_rate, self.sample_rate);
        }
        let attack = self.params.goal(self.index.env_attack);
        let release = self.params.goal(self.index.env_release);
        if (attack, release) != self.env_times {
            self.env_times = (attack, release);
            self.envelope.set_times(attack, release, self.sample_rate);
        }

        let [left, right] = ctx.buffers.as_mut_slices();
        let mut cutoff_now = self.live_cutoff;

        for frame in 0..frames {
            let cutoff = self.params.next(self.index.cutoff);
            let res = self.params.next(self.index.res);
            let lfo_amount = self.params.next(self.index.lfo_amount) * 0.01;
            let spread = self.params.next(self.index.spread) * 0.01;
            let env_amount = self.params.next(self.index.env_amount) * 0.01;
            let mix = self.params.next(self.index.mix) * 0.01;
            // Block-rate parameters still have to walk, or "settled" would never
            // come true and the transparency check would never fire.
            self.params.next(self.index.lfo_rate);
            self.params.next(self.index.env_attack);
            self.params.next(self.index.env_release);

            let dry_l = left[frame];
            let dry_r = right[frame];

            let level = self.envelope.process(dry_l.abs().max(dry_r.abs()));
            self.lfo.advance();

            if self.control == 0 {
                let env = (level / ENV_REFERENCE).min(1.0) * env_amount * MAX_ENV_OCTAVES;
                let swing = lfo_amount * MAX_LFO_OCTAVES;
                let q = resonance_q(res);
                let left_hz = cutoff * (swing * self.lfo.sine_at(spread * 0.25) + env).exp2();
                let right_hz = cutoff * (swing * self.lfo.sine_at(-spread * 0.25) + env).exp2();
                self.filters[0].set(left_hz, q, self.sample_rate);
                self.filters[1].set(right_hz, q, self.sample_rate);
                cutoff_now = left_hz.clamp(MIN_CUTOFF_HZ * 0.5, self.sample_rate * 0.49);
            }
            self.control = (self.control + 1) % CONTROL_INTERVAL;

            let wet_l = self.filters[0].mode(dry_l, mode);
            let wet_r = self.filters[1].mode(dry_r, mode);

            left[frame] = dry_l * (1.0 - mix) + wet_l * mix;
            right[frame] = dry_r * (1.0 - mix) + wet_r * mix;
        }

        self.live_cutoff = cutoff_now;
        self.publish(cutoff_now, self.envelope.value());
    }

    /// Keep the filter, the follower and the oscillator current while the
    /// output is the input.
    ///
    /// The transparency fast path writes the dry signal back, but a filter that
    /// has not seen the last two seconds of audio would start from silence the
    /// moment somebody nudges the resonance -- and the ear hears a cold filter
    /// engaging as a thump, which is exactly the class of artefact
    /// `tests/determinism.rs` slams every parameter looking for. Ten multiplies
    /// a sample is a cheap price for the knob being smooth the first time it is
    /// touched.
    fn warm(&mut self, ctx: &ProcessCtx) {
        let [left, right] = ctx.buffers.as_slices();
        let frames = ctx.frames.min(left.len()).min(right.len());
        for frame in 0..frames {
            self.envelope
                .process(left[frame].abs().max(right[frame].abs()));
            self.lfo.advance();
            self.filters[0].process(left[frame]);
            self.filters[1].process(right[frame]);
        }
    }

    fn publish(&self, cutoff: f32, envelope: f32) {
        let block = self.params.block();
        block.set_readout(READOUT_CUTOFF, cutoff);
        block.set_readout(READOUT_ENV, (envelope / ENV_REFERENCE).min(1.0));
    }
}

impl RtNode for AutoFilterNode {
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
                self.warm(ctx);
                self.live_cutoff = self.params.value(self.index.cutoff);
                self.publish(self.live_cutoff, self.envelope.value());
            } else {
                self.render(ctx);
            }
        } else {
            self.params.skip_all(ctx.frames);
            self.publish(self.params.value(self.index.cutoff), 0.0);
        }

        self.bypass.blend(ctx, state, bypassed);
    }
}

/// Builds [`AutoFilterNode`]s.
#[derive(Debug, Default)]
pub struct AutoFilterFactory;

impl AutoFilterFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for AutoFilterFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for AutoFilterFactory {
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
        Box::new(AutoFilterNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{level_db_at, render_mono, render_stereo, silence, sine, Harness};

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&AutoFilterFactory::new(), SR, 512)
    }

    /// Level of a steady tone through a fresh filter at `cutoff`, in dB.
    fn tone_db(h: &mut Harness, hz: f64) -> f64 {
        let signal = sine(hz, 48_000, SR, 0.5);
        render_mono(h, &signal);
        let out = render_mono(h, &signal);
        level_db_at(&out[24_000..], hz, SR)
    }

    /// The level of an unprocessed 0.5 sine, for comparison.
    const UNITY_DB: f64 = -6.020_6;

    #[test]
    fn a_fresh_auto_filter_is_bit_transparent() {
        // Wide open, no resonance, no modulation: the filter is out of the path
        // rather than running at 20 kHz.
        let mut h = harness();
        let input: Vec<[f32; 2]> = (0..2_048)
            .map(|n| {
                let x = (n as f32 * 0.037).sin() * 0.6;
                [x, x * 0.3 - 0.1]
            })
            .collect();
        assert_eq!(render_stereo(&mut h, &input), input);
    }

    #[test]
    fn the_lowpass_passes_below_the_cutoff_and_stops_above_it() {
        let mut h = harness();
        h.set("cutoff", 500.0);
        assert!(
            (tone_db(&mut h, 100.0) - UNITY_DB).abs() < 0.6,
            "the passband is not flat"
        );

        let mut h = harness();
        h.set("cutoff", 500.0);
        let stopped = tone_db(&mut h, 5_000.0);
        // Two poles: 40 dB per decade, so 24 dB down at ten times the cutoff is
        // the least it can be.
        assert!(
            stopped < UNITY_DB - 30.0,
            "5 kHz through a 500 Hz lowpass came out at {stopped:.1} dB"
        );

        // Minus three decibels at the corner, which is what "cutoff" means.
        let mut h = harness();
        h.set("cutoff", 500.0);
        let corner = tone_db(&mut h, 500.0);
        assert!(
            (corner - (UNITY_DB - 3.0)).abs() < 1.0,
            "the corner is {corner:.1} dB, not {:.1}",
            UNITY_DB - 3.0
        );
    }

    #[test]
    fn the_highpass_and_the_bandpass_are_the_other_two_outputs_of_the_same_filter() {
        let mut h = harness();
        h.set("mode", 1.0);
        h.set("cutoff", 1_000.0);
        assert!(tone_db(&mut h, 100.0) < UNITY_DB - 30.0);
        let mut h = harness();
        h.set("mode", 1.0);
        h.set("cutoff", 1_000.0);
        assert!((tone_db(&mut h, 10_000.0) - UNITY_DB).abs() < 1.0);

        // The bandpass peaks *at* the cutoff and falls away either side, and it
        // is normalised so its peak is unity rather than Q.
        let mut h = harness();
        h.set("mode", 2.0);
        h.set("cutoff", 1_000.0);
        let peak = tone_db(&mut h, 1_000.0);
        assert!(
            (peak - UNITY_DB).abs() < 0.6,
            "the bandpass peak is {peak:.1} dB"
        );
        let mut h = harness();
        h.set("mode", 2.0);
        h.set("cutoff", 1_000.0);
        assert!(tone_db(&mut h, 100.0) < peak - 15.0);
    }

    #[test]
    fn resonance_puts_a_peak_at_the_corner_without_moving_it() {
        let mut flat = harness();
        flat.set("cutoff", 1_000.0);
        let no_res = tone_db(&mut flat, 1_000.0);

        let mut sharp = harness();
        sharp.set("cutoff", 1_000.0);
        sharp.set("res", 100.0);
        let full_res = tone_db(&mut sharp, 1_000.0);
        assert!(
            full_res > no_res + 12.0,
            "resonance did nothing: {full_res:.1} against {no_res:.1}"
        );
        // The Q the knob claims: a two-pole lowpass peaks at about Q, so the
        // corner comes back 20·log10(Q) above unity.
        let expected = UNITY_DB + 20.0 * f64::from(MAX_Q).log10();
        assert!(
            (full_res - expected).abs() < 3.0,
            "a Q of {MAX_Q} should peak near {expected:.1} dB, got {full_res:.1}"
        );
        // ...and it did not drag the corner with it.
        let mut sharp = harness();
        sharp.set("cutoff", 1_000.0);
        sharp.set("res", 100.0);
        assert!(tone_db(&mut sharp, 8_000.0) < UNITY_DB - 24.0);
    }

    #[test]
    fn the_lfo_sweeps_the_cutoff_across_the_probe() {
        let mut h = harness();
        h.set("cutoff", 800.0);
        h.set("lfo_amount", 100.0);
        h.set("lfo_rate", 2.0);
        let signal = sine(2_000.0, 48_000, SR, 0.5);
        render_mono(&mut h, &signal);
        let out = render_mono(&mut h, &signal);
        let levels: Vec<f64> = (0..4)
            .map(|k| level_db_at(&out[k * 6_000..(k + 1) * 6_000], 2_000.0, SR))
            .collect();
        let spread = levels.iter().cloned().fold(f64::MIN, f64::max)
            - levels.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            spread > 20.0,
            "a three-octave sweep should swing the probe by tens of dB: {levels:?}"
        );
        // At zero amount it is a still filter again.
        let mut h = harness();
        h.set("cutoff", 800.0);
        h.set("lfo_amount", 0.0);
        h.set("lfo_rate", 2.0);
        render_mono(&mut h, &signal); // let the cutoff glide arrive
        let out = render_mono(&mut h, &signal);
        let levels: Vec<f64> = (0..4)
            .map(|k| level_db_at(&out[k * 6_000..(k + 1) * 6_000], 2_000.0, SR))
            .collect();
        let spread = levels.iter().cloned().fold(f64::MIN, f64::max)
            - levels.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            spread < 3.0,
            "the LFO is still running at zero amount: {levels:?}"
        );
    }

    #[test]
    fn a_synced_lfo_runs_at_the_tempo_the_transport_reports() {
        // The mapping first, against the arithmetic.
        assert!(
            (sync_rate_hz(2, 120.0) - 2.0).abs() < 1e-6,
            "1/4 at 120 BPM"
        );
        assert!(
            (sync_rate_hz(3, 120.0) - 4.0).abs() < 1e-6,
            "1/8 at 120 BPM"
        );
        assert!((sync_rate_hz(2, 60.0) - 1.0).abs() < 1e-6);

        // Then the audio: a 1/4-note sweep at 120 BPM completes two cycles a
        // second, so a half-second window holds exactly one.
        let mut h = harness().at_bpm(120.0);
        h.set("cutoff", 700.0);
        h.set("lfo_amount", 100.0);
        h.set("lfo_sync", 1.0);
        h.set("lfo_div", 2.0);
        let signal = sine(2_000.0, 48_000, SR, 0.5);
        render_mono(&mut h, &signal);
        let out = render_mono(&mut h, &signal);
        // Sixteen windows over one second: the envelope of the probe should
        // rise and fall twice.
        let levels: Vec<f64> = (0..16)
            .map(|k| level_db_at(&out[k * 3_000..(k + 1) * 3_000], 2_000.0, SR))
            .collect();
        let mut crossings = 0;
        let mid = (levels.iter().cloned().fold(f64::MIN, f64::max)
            + levels.iter().cloned().fold(f64::MAX, f64::min))
            / 2.0;
        for pair in levels.windows(2) {
            if (pair[0] - mid).signum() != (pair[1] - mid).signum() {
                crossings += 1;
            }
        }
        assert!(
            (3..=5).contains(&crossings),
            "two cycles a second is four crossings of the midpoint, got {crossings}: {levels:?}"
        );
    }

    #[test]
    fn the_envelope_follower_opens_the_filter_for_loud_input_and_closes_it_for_quiet() {
        // The auto-wah claim, measured as a *ratio*: the same tone at two levels
        // through the same settings must come out with different tone, not just
        // different level.
        let brightness = |amplitude: f32| {
            let mut h = harness();
            h.set("cutoff", 300.0);
            h.set("env_amount", 100.0);
            h.set("env_attack", 5.0);
            h.set("env_release", 200.0);
            let signal: Vec<f32> = (0..48_000)
                .map(|n| {
                    let t = n as f64 / SR;
                    amplitude
                        * (((std::f64::consts::TAU * 120.0 * t).sin()
                            + (std::f64::consts::TAU * 3_000.0 * t).sin())
                            * 0.5) as f32
                })
                .collect();
            render_mono(&mut h, &signal);
            let out = render_mono(&mut h, &signal);
            level_db_at(&out[24_000..], 3_000.0, SR) - level_db_at(&out[24_000..], 120.0, SR)
        };
        let loud = brightness(0.9);
        let quiet = brightness(0.02);
        assert!(
            loud > quiet + 12.0,
            "the envelope did not open the filter: loud {loud:.1} dB, quiet {quiet:.1} dB"
        );

        // A negative amount does the opposite, which is the setting a busy bus
        // wants.
        let mut h = harness();
        h.set("cutoff", 2_000.0);
        h.set("env_amount", -100.0);
        let loud_signal = sine(1_500.0, 48_000, SR, 0.9);
        render_mono(&mut h, &loud_signal);
        let out = render_mono(&mut h, &loud_signal);
        let closed = level_db_at(&out[24_000..], 1_500.0, SR);
        assert!(
            closed < 20.0 * 0.9_f64.log10() - 20.0,
            "a negative amount should have closed the filter, got {closed:.1} dB"
        );
    }

    #[test]
    fn the_cutoff_and_envelope_readouts_report_what_the_filter_is_doing() {
        let mut h = harness();
        h.set("cutoff", 1_000.0);
        h.set("lfo_amount", 0.0);
        h.set("env_amount", 0.0);
        h.set("res", 20.0);
        render_mono(&mut h, &sine(200.0, 4_096, SR, 0.5));
        assert!(
            (h.readout("cutoff") - 1_000.0).abs() < 20.0,
            "unmodulated, the readout is the knob: {}",
            h.readout("cutoff")
        );

        // With the envelope wide open the reported cutoff has to leave the knob
        // behind, and the envelope readout has to be at its ceiling.
        let mut h = harness();
        h.set("cutoff", 300.0);
        h.set("env_amount", 100.0);
        render_mono(&mut h, &sine(200.0, 24_000, SR, 0.9));
        assert!(
            h.readout("cutoff") > 1_500.0,
            "the modulated cutoff is {}",
            h.readout("cutoff")
        );
        assert!(
            (h.readout("env") - 1.0).abs() < 0.05,
            "{}",
            h.readout("env")
        );

        // Silence lets it back down.
        render_mono(&mut h, &silence(48_000));
        assert!(h.readout("env") < 0.05, "{}", h.readout("env"));
    }

    #[test]
    fn spread_puts_the_two_sides_at_different_cutoffs() {
        let mut h = harness();
        h.set("cutoff", 700.0);
        h.set("lfo_amount", 100.0);
        h.set("lfo_rate", 0.01);
        h.set("spread", 100.0);
        // A quarter turn apart at 100 %: one side is at the top of the sweep
        // while the other is in the middle.
        let signal: Vec<[f32; 2]> = sine(2_500.0, 48_000, SR, 0.5)
            .into_iter()
            .map(|s| [s, s])
            .collect();
        render_stereo(&mut h, &signal);
        let out = render_stereo(&mut h, &signal);
        let left: Vec<f32> = out[24_000..].iter().map(|f| f[0]).collect();
        let right: Vec<f32> = out[24_000..].iter().map(|f| f[1]).collect();
        let gap = (level_db_at(&left, 2_500.0, SR) - level_db_at(&right, 2_500.0, SR)).abs();
        assert!(gap > 4.0, "the two sides are only {gap:.1} dB apart");
    }

    #[test]
    fn mix_blends_the_filter_against_the_untouched_input() {
        let mut h = harness();
        h.set("cutoff", 200.0);
        h.set(MIX_PARAM, 0.0);
        let level = tone_db(&mut h, 5_000.0);
        assert!(
            (level - UNITY_DB).abs() < 0.1,
            "at 0 % mix nothing should be filtered, got {level:.1} dB"
        );
    }

    #[test]
    fn a_bypassed_auto_filter_is_the_input_again() {
        let mut h = harness();
        h.set("cutoff", 400.0);
        h.set("res", 60.0);
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
