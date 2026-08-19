//! `effects.utility` -- gain, pan, width, mono, polarity and a DC blocker.
//!
//! The effect that does the boring things a mixer strip cannot: flip a
//! polarity, collapse a stereo image, take a constant offset out of a signal
//! somebody's plugin put there. Every one of them is two multiplies, and having
//! them behind a knob is worth far more than the arithmetic costs.
//!
//! # The pan law is the mixer's pan law
//! Equal power, normalised so centre is unity -- exactly
//! [`pan_gains`](mydaw_rt_core::pan_gains), the same function the track strips
//! call. The research says the *researched* compromise is a -4.5 dB law, and
//! that mismatched pan laws are "one common reason why mixes constructed on
//! different DAWs but with apparently identical fader and pan control settings
//! can sound subtly but noticeably different". That finding is precisely why
//! this does not introduce a second one: two pan laws inside a single DAW would
//! be a worse version of the same problem, in a place where a user has even less
//! reason to suspect it. A pan-law *choice* belongs to the project, read by the
//! mixer and this effect alike, and is not an effect's business.
//!
//! # Width, and why 100 % is bit-transparent
//! Mid/side with the conventional halves: `M = (L+R)/2`, `S = (L-R)/2`,
//! `L = M+S`, `R = M-S`. Reconstruction is exact in real arithmetic but *not* in
//! `f32` -- the two sums round independently, so a round trip at width 100 %
//! would leave a few ulps behind. Users null-test this. So the mid/side stage is
//! **skipped entirely** when width is settled at 100 % with mono off, which
//! makes the transparency exact rather than nearly exact, and is also the fast
//! path for the overwhelmingly common case.
//!
//! Width 0 % is a plain mono sum at `-6 dB` per channel and deliberately *not*
//! power-compensated: engineers use it as a mono-compatibility check and expect
//! centred content to keep its level.

use mydaw_rt_core::{
    pan_gains, ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode,
    RtNodeFactory, SmoothedParams, CHANNELS,
};
use std::sync::Arc;

use crate::dsp::DcBlocker;
use crate::effect::{index_of, Bypass, CachedDb, BYPASS_PARAM};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.utility";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "Utility";

/// One line for a tooltip.
pub const DESCRIPTION: &str = "Gain, pan, stereo width, mono, polarity and a DC blocker";

/// Widest trim this offers, in dB. Bitwig's range; ±12 dB is the most-complained
/// about trim range in any DAW.
pub const MAX_GAIN_DB: f32 = 36.0;

/// What the utility exposes.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::new(
        "gain",
        "Gain",
        "dB",
        -MAX_GAIN_DB,
        MAX_GAIN_DB,
        0.0,
        ParamCurve::Db,
    ),
    ParamDef::new("pan", "Pan", "", -1.0, 1.0, 0.0, ParamCurve::Linear),
    ParamDef::new("width", "Width", "%", 0.0, 200.0, 100.0, ParamCurve::Linear),
    ParamDef::toggle("mono", "Mono", false),
    ParamDef::toggle("invert_l", "Invert L", false),
    ParamDef::toggle("invert_r", "Invert R", false),
    ParamDef::toggle("dc_block", "DC Filter", false),
];

/// Parameter indices, resolved once off the audio thread.
#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    gain: usize,
    pan: usize,
    width: usize,
    mono: usize,
    invert_l: usize,
    invert_r: usize,
    dc_block: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        Self {
            bypass: index_of(block, BYPASS_PARAM),
            gain: index_of(block, "gain"),
            pan: index_of(block, "pan"),
            width: index_of(block, "width"),
            mono: index_of(block, "mono"),
            invert_l: index_of(block, "invert_l"),
            invert_r: index_of(block, "invert_r"),
            dc_block: index_of(block, "dc_block"),
        }
    }
}

/// A pan position cached as the two channel gains it means.
///
/// `pan_gains` is a `cos` and a `sin`; caching it costs one compare per sample
/// and saves both whenever the knob is still.
#[derive(Debug, Clone, Copy)]
struct CachedPan {
    pan: f32,
    gains: [f32; CHANNELS],
}

impl CachedPan {
    fn new(pan: f32) -> Self {
        Self {
            pan,
            gains: pan_gains(pan),
        }
    }

    #[inline]
    fn next(&mut self, pan: f32) -> [f32; CHANNELS] {
        if pan != self.pan {
            self.pan = pan;
            self.gains = pan_gains(pan);
        }
        self.gains
    }
}

/// The utility node.
pub struct UtilityNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    gain: CachedDb,
    pan: CachedPan,
    dc: [DcBlocker; CHANNELS],
}

impl UtilityNode {
    /// **Non-RT**: allocates.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let index = Indices::new(&params);
        let smoothed = SmoothedParams::new(params, sample_rate);
        let rate = crate::dsp::usable_sample_rate(sample_rate);
        Self {
            gain: CachedDb::new(index.gain, &smoothed),
            pan: CachedPan::new(smoothed.value(index.pan)),
            dc: [DcBlocker::new(DcBlocker::DEFAULT_HZ, rate); CHANNELS],
            bypass: Bypass::new(max_block, sample_rate),
            params: smoothed,
            index,
        }
    }

    fn reset(&mut self) {
        for blocker in &mut self.dc {
            blocker.reset();
        }
    }

    /// Whether this block can be skipped entirely.
    ///
    /// The transparency claim: at its defaults the utility is not "very nearly"
    /// a pass-through, it *is* one, because nothing multiplies anything.
    fn is_transparent(&self) -> bool {
        let settled = |index: usize| !self.params.is_smoothing(index);
        self.params.choice(self.index.mono) == 0
            && self.params.choice(self.index.invert_l) == 0
            && self.params.choice(self.index.invert_r) == 0
            && self.params.choice(self.index.dc_block) == 0
            && settled(self.index.gain)
            && self.params.value(self.index.gain) == 0.0
            && settled(self.index.pan)
            && self.params.value(self.index.pan) == 0.0
            && settled(self.index.width)
            && self.params.value(self.index.width) == 100.0
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        let mono = self.params.choice(self.index.mono) != 0;
        let sign_l = if self.params.choice(self.index.invert_l) != 0 {
            -1.0
        } else {
            1.0
        };
        let sign_r = if self.params.choice(self.index.invert_r) != 0 {
            -1.0
        } else {
            1.0
        };
        let dc_on = self.params.choice(self.index.dc_block) != 0;
        // The mid/side stage is skipped whole when it would be an identity, so
        // width 100 % is exactly transparent rather than nearly so.
        let ms_needed = mono
            || self.params.is_smoothing(self.index.width)
            || self.params.value(self.index.width) != 100.0;

        let channels = ctx.buffers.as_mut_slices();
        let [left, right] = channels;
        for frame in 0..frames {
            let width = self.params.next(self.index.width) * 0.01;
            let gain = self.gain.next(&mut self.params);
            let pan = self.pan.next(self.params.next(self.index.pan));

            let mut l = left[frame] * sign_l;
            let mut r = right[frame] * sign_r;

            if dc_on {
                l = self.dc[0].process(l);
                r = self.dc[1].process(r);
            }

            if ms_needed {
                let mid = (l + r) * 0.5;
                let side = if mono { 0.0 } else { (l - r) * 0.5 * width };
                l = mid + side;
                r = mid - side;
            }

            left[frame] = l * gain * pan[0];
            right[frame] = r * gain * pan[1];
        }
    }
}

impl RtNode for UtilityNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        self.params.begin_block();
        let bypassed = self.params.choice(self.index.bypass) != 0;
        let state = self.bypass.begin(ctx, bypassed);

        if state.processes() {
            if state.resets() {
                self.reset();
            }
            if self.is_transparent() {
                // Nothing to do, and nothing done: the buffer keeps the exact
                // bits it arrived with.
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

/// Builds [`UtilityNode`]s.
#[derive(Debug, Default)]
pub struct UtilityFactory;

impl UtilityFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for UtilityFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for UtilityFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(UtilityNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{db, render_stereo, rms, Harness};

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&UtilityFactory::new(), SR, 512)
    }

    #[test]
    fn a_fresh_utility_is_bit_transparent() {
        // The rule for every effect in the pack: freshly inserted, it does
        // nothing at all -- not "almost nothing".
        let mut h = harness();
        let input: Vec<[f32; 2]> = (0..256)
            .map(|n| {
                let x = (n as f32 * 0.037).sin() * 0.7;
                [x, x * 0.3 - 0.11]
            })
            .collect();
        let out = render_stereo(&mut h, &input);
        assert_eq!(out, input, "a default utility must be a pass-through");
    }

    #[test]
    fn gain_is_exactly_the_decibels_it_says() {
        let mut h = harness();
        h.set("gain", -6.0206);
        let input = vec![[1.0_f32, 1.0]; 4_096];
        let out = render_stereo(&mut h, &input);
        // Past the 10 ms ramp.
        assert!((out[2_000][0] - 0.5).abs() < 1e-4, "{}", out[2_000][0]);
        assert!((out[2_000][1] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn pan_uses_the_mixers_law_and_nothing_else() {
        let mut h = harness();
        h.set("pan", -1.0);
        let input = vec![[1.0_f32, 1.0]; 4_096];
        let out = render_stereo(&mut h, &input);
        let expected = pan_gains(-1.0);
        assert!((out[3_000][0] - expected[0]).abs() < 1e-5);
        assert!(out[3_000][1].abs() < 1e-6, "hard left leaks right");

        // Centre is unity in both channels: the property the whole "a mixer was
        // added and nothing got quieter" story rests on.
        let centre = pan_gains(0.0);
        assert!((centre[0] - 1.0).abs() < 1e-6 && (centre[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn width_zero_is_a_plain_mono_sum_and_two_hundred_doubles_the_sides() {
        let mut h = harness();
        h.set("width", 0.0);
        // Hard-panned content: all side, no mid.
        let input = vec![[1.0_f32, -1.0]; 4_096];
        let out = render_stereo(&mut h, &input);
        assert!(out[3_000][0].abs() < 1e-5, "opposed channels must cancel");

        // Centred content keeps its level: no power compensation at 0 %.
        let mut h = harness();
        h.set("width", 0.0);
        let input = vec![[0.5_f32, 0.5]; 4_096];
        let out = render_stereo(&mut h, &input);
        assert!((out[3_000][0] - 0.5).abs() < 1e-5, "{}", out[3_000][0]);

        let mut h = harness();
        h.set("width", 200.0);
        let input = vec![[0.75_f32, 0.25]; 4_096];
        let out = render_stereo(&mut h, &input);
        // M = 0.5, S = 0.25 -> doubled S = 0.5 -> L = 1.0, R = 0.0.
        assert!((out[3_000][0] - 1.0).abs() < 1e-4, "{}", out[3_000][0]);
        assert!(out[3_000][1].abs() < 1e-4, "{}", out[3_000][1]);
    }

    #[test]
    fn width_one_hundred_percent_is_bit_transparent_rather_than_nearly_so() {
        // Users null-test this. `(L+R)/2 + (L-R)/2` is exact in real arithmetic
        // and not in f32, so the mid/side stage has to be skipped, not merely
        // configured to an identity.
        let mut h = harness();
        h.set("width", 100.0);
        let input: Vec<[f32; 2]> = (0..512)
            .map(|n| [(n as f32 * 0.011).sin(), (n as f32 * 0.019).cos() * 0.31])
            .collect();
        assert_eq!(render_stereo(&mut h, &input), input);
    }

    #[test]
    fn mono_collapses_without_disturbing_the_width_knob() {
        let mut h = harness();
        h.set("mono", 1.0);
        let input = vec![[1.0_f32, 0.0]; 1_024];
        let out = render_stereo(&mut h, &input);
        assert!((out[512][0] - 0.5).abs() < 1e-5);
        assert!((out[512][1] - 0.5).abs() < 1e-5);
        assert_eq!(
            h.get("width"),
            100.0,
            "a mono check must not destroy the width that was dialled"
        );
    }

    #[test]
    fn polarity_inverts_one_channel_at_a_time() {
        let mut h = harness();
        h.set("invert_l", 1.0);
        let input = vec![[0.25_f32, 0.25]; 512];
        let out = render_stereo(&mut h, &input);
        assert_eq!(out[100], [-0.25, 0.25]);

        let mut h = harness();
        h.set("invert_r", 1.0);
        let out = render_stereo(&mut h, &input);
        assert_eq!(out[100], [0.25, -0.25]);
    }

    #[test]
    fn the_dc_blocker_removes_an_offset_and_is_off_by_default() {
        let mut h = harness();
        let input = vec![[0.5_f32, 0.5]; 4_096];
        let out = render_stereo(&mut h, &input);
        assert_eq!(out[4_000][0], 0.5, "off by default: transparency first");

        // A 5 Hz corner settles in about 80 ms, so the offset is measured a
        // quarter of a second in rather than at the top of the first block.
        h.set("dc_block", 1.0);
        render_stereo(&mut h, &input);
        render_stereo(&mut h, &input);
        let out = render_stereo(&mut h, &input);
        assert!(
            out[4_000][0].abs() < 0.005,
            "the offset should be gone, got {}",
            out[4_000][0]
        );
    }

    #[test]
    fn a_gain_step_ramps_in_decibels_rather_than_clicking() {
        // The ramp is linear **in dB**, not in amplitude, because dB is the
        // scale the knob is drawn on and the one the ear hears: a constant
        // dB-per-second fade sounds even, and a constant amplitude-per-second
        // one does not. So the thing to bound is the decibel step, and a DC
        // input makes the output *be* the coefficient, so the step is visible
        // directly.
        let mut h = harness();
        let input = vec![[1.0_f32, 1.0]; 2_048];
        render_stereo(&mut h, &input);
        h.set("gain", -MAX_GAIN_DB);
        let out = render_stereo(&mut h, &input);

        let worst = out
            .windows(2)
            .map(|pair| (db(pair[0][0]) - db(pair[1][0])).abs())
            .fold(0.0_f32, f32::max);
        let limit = MAX_GAIN_DB / 480.0 + 1e-3;
        assert!(
            worst <= limit,
            "the gain stepped by {worst} dB (limit {limit})"
        );
        // ...and it arrives, rather than asymptoting.
        assert!((db(out[1_000][0]) + MAX_GAIN_DB).abs() < 0.01);
        // Monotone throughout: a ramp that overshoots is a different click.
        assert!(out.windows(2).all(|p| p[1][0] <= p[0][0] + 1e-7));
    }

    #[test]
    fn a_bypassed_utility_is_the_input_again() {
        let mut h = harness();
        h.set("gain", 12.0);
        h.set("width", 0.0);
        let input: Vec<[f32; 2]> = (0..4_096)
            .map(|n| [(n as f32 * 0.03).sin(), (n as f32 * 0.07).sin()])
            .collect();
        let loud = render_stereo(&mut h, &input);
        assert!(rms(&loud) > rms(&input) * 1.5);

        h.set("bypass", 1.0);
        render_stereo(&mut h, &input); // the crossfade
        let out = render_stereo(&mut h, &input);
        assert_eq!(out, input, "a settled bypass is the input, bit for bit");
    }
}
