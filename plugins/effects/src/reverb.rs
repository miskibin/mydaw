//! `effects.reverb` -- a stereo algorithmic reverb (Freeverb-family).
//!
//! ```text
//!   in → predelay → 4 combs (damped) → 2 allpasses → width → mix → out
//! ```
//!
//! Mix defaults to 25 %, like the delay: a reverb that starts fully dry looks
//! broken. Decay is RT60 of the longest comb. Damp darkens each repeat. Size
//! scales the comb lengths. Width is mid/side of the wet only.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode, RtNodeFactory,
    SmoothedParams,
};

use crate::dsp::{usable_sample_rate, DelayLine, OnePoleTpt};
use crate::effect::{index_of, Bypass, BYPASS_PARAM, MIX_PARAM};

pub const NODE_TYPE_ID: &str = "effects.reverb";
pub const DISPLAY_NAME: &str = "Reverb";
pub const DESCRIPTION: &str =
    "Algorithmic room: decay, size, damp and mix. The display is the space.";

const COMB_BASE: [f32; 4] = [1116.0, 1188.0, 1277.0, 1356.0];
const ALLPASS_BASE: [f32; 2] = [556.0, 441.0];
const STEREO_SPREAD: f32 = 23.0;
const BASE_SR: f32 = 44_100.0;
const MAX_PREDELAY_MS: f32 = 80.0;

pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::new(MIX_PARAM, "Mix", "%", 0.0, 100.0, 25.0, ParamCurve::Linear),
    ParamDef::new("decay", "Decay", "s", 0.2, 12.0, 1.6, ParamCurve::Log),
    ParamDef::new("size", "Size", "%", 20.0, 150.0, 80.0, ParamCurve::Linear).smoothed(80.0),
    ParamDef::new("damp", "Damp", "%", 0.0, 100.0, 40.0, ParamCurve::Linear),
    ParamDef::new(
        "predelay",
        "Predelay",
        "ms",
        0.0,
        MAX_PREDELAY_MS,
        12.0,
        ParamCurve::Linear,
    )
    .smoothed(80.0),
    ParamDef::new("width", "Width", "%", 0.0, 100.0, 100.0, ParamCurve::Linear),
];

#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    mix: usize,
    decay: usize,
    size: usize,
    damp: usize,
    predelay: usize,
    width: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        Self {
            bypass: index_of(block, BYPASS_PARAM),
            mix: index_of(block, MIX_PARAM),
            decay: index_of(block, "decay"),
            size: index_of(block, "size"),
            damp: index_of(block, "damp"),
            predelay: index_of(block, "predelay"),
            width: index_of(block, "width"),
        }
    }
}

struct Comb {
    line: DelayLine,
    delay: f32,
    /// Added to `delay` each sample, gliding it to the block's target.
    delay_step: f32,
    filter: OnePoleTpt,
}

impl Comb {
    fn new(frames: usize, delay: f32) -> Self {
        Self {
            line: DelayLine::new(frames),
            delay: delay.max(DelayLine::MIN_DELAY),
            delay_step: 0.0,
            filter: OnePoleTpt::new(),
        }
    }

    fn clear(&mut self) {
        self.line.clear();
        self.filter.reset();
    }

    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        self.delay += self.delay_step;
        self.filter.set_coefficient(damp);
        let y = self.line.read(self.delay);
        let lp = self.filter.low(y);
        self.line.write(x + lp * feedback);
        y
    }
}

struct Allpass {
    line: DelayLine,
    delay: f32,
    /// Added to `delay` each sample, gliding it to the block's target.
    delay_step: f32,
}

impl Allpass {
    fn new(frames: usize, delay: f32) -> Self {
        Self {
            line: DelayLine::new(frames),
            delay: delay.max(DelayLine::MIN_DELAY),
            delay_step: 0.0,
        }
    }

    fn clear(&mut self) {
        self.line.clear();
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.delay += self.delay_step;
        let buf = self.line.read(self.delay);
        let y = -x + buf;
        self.line.write(x + buf * 0.5);
        y
    }
}

struct Channel {
    combs: [Comb; 4],
    allpasses: [Allpass; 2],
}

impl Channel {
    fn new(sample_rate: f32, spread: f32) -> Self {
        let scale = sample_rate / BASE_SR;
        let comb_cap = ((COMB_BASE[3] + spread) * scale * 1.6) as usize + 32;
        let ap_cap = ((ALLPASS_BASE[0] + spread) * scale * 1.6) as usize + 32;
        Self {
            combs: std::array::from_fn(|i| Comb::new(comb_cap, (COMB_BASE[i] + spread) * scale)),
            allpasses: std::array::from_fn(|i| {
                Allpass::new(ap_cap, (ALLPASS_BASE[i] + spread) * scale)
            }),
        }
    }

    fn clear(&mut self) {
        for comb in &mut self.combs {
            comb.clear();
        }
        for ap in &mut self.allpasses {
            ap.clear();
        }
    }

    /// Land every delay on the length `size` names. For the first block after
    /// a build or a reset, where a glide from a stale length would make the
    /// output depend on the block size.
    fn set_size(&mut self, sample_rate: f32, spread: f32, size: f32) {
        let scale = (sample_rate / BASE_SR) * size;
        for (i, comb) in self.combs.iter_mut().enumerate() {
            comb.delay = ((COMB_BASE[i] + spread) * scale).max(DelayLine::MIN_DELAY);
            comb.delay_step = 0.0;
        }
        for (i, ap) in self.allpasses.iter_mut().enumerate() {
            ap.delay = ((ALLPASS_BASE[i] + spread) * scale).max(DelayLine::MIN_DELAY);
            ap.delay_step = 0.0;
        }
    }

    /// Aim every delay at the length `size` names, reaching it over `frames`
    /// samples -- one add per sample instead of a per-sample recompute.
    fn glide_size(&mut self, sample_rate: f32, spread: f32, size: f32, frames: usize) {
        let scale = (sample_rate / BASE_SR) * size;
        let inv = 1.0 / frames.max(1) as f32;
        for (i, comb) in self.combs.iter_mut().enumerate() {
            let target = ((COMB_BASE[i] + spread) * scale).max(DelayLine::MIN_DELAY);
            comb.delay_step = (target - comb.delay) * inv;
        }
        for (i, ap) in self.allpasses.iter_mut().enumerate() {
            let target = ((ALLPASS_BASE[i] + spread) * scale).max(DelayLine::MIN_DELAY);
            ap.delay_step = (target - ap.delay) * inv;
        }
    }

    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let mut acc = 0.0;
        for comb in &mut self.combs {
            acc += comb.process(x, feedback, damp);
        }
        acc *= 0.25;
        for ap in &mut self.allpasses {
            acc = ap.process(acc);
        }
        acc
    }
}

/// Feedback for RT60 given the longest comb delay in samples.
fn feedback_for_rt60(delay_samples: f32, rt60: f32, sample_rate: f32) -> f32 {
    let delay_s = delay_samples / sample_rate.max(1.0);
    let rt60 = rt60.max(0.05);
    10.0_f32.powf(-3.0 * delay_s / rt60).clamp(0.0, 0.98)
}

pub struct ReverbNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    pre_l: DelayLine,
    pre_r: DelayLine,
    left: Channel,
    right: Channel,
    sample_rate: f32,
    /// Comb feedback, gliding per sample toward a block-rate target.
    feedback: f32,
    feedback_step: f32,
    /// Damping coefficient, gliding the same way. Shared by all eight combs.
    damp: f32,
    damp_step: f32,
    /// Whether the glides have a real starting point yet. False after a build
    /// or a reset, where the first block lands on its targets instead.
    primed: bool,
}

impl ReverbNode {
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let sr = usable_sample_rate(sample_rate);
        let pre_cap = ((MAX_PREDELAY_MS * 0.001 * sr) as usize).max(8) + 8;
        let smoothed = SmoothedParams::new(Arc::clone(&params), sample_rate);
        let index = Indices::new(&params);
        Self {
            index,
            params: smoothed,
            bypass: Bypass::new(max_block, sample_rate),
            pre_l: DelayLine::new(pre_cap),
            pre_r: DelayLine::new(pre_cap),
            left: Channel::new(sr, 0.0),
            right: Channel::new(sr, STEREO_SPREAD),
            sample_rate: sr,
            feedback: 0.0,
            feedback_step: 0.0,
            damp: 0.0,
            damp_step: 0.0,
            primed: false,
        }
    }

    fn reset(&mut self) {
        self.pre_l.clear();
        self.pre_r.clear();
        self.left.clear();
        self.right.clear();
        self.primed = false;
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        if frames == 0 {
            return;
        }

        // Expensive coefficients are aimed at block rate and glided per sample
        // (the crate convention -- see `OnePoleTpt::set_cutoff`): the decay's
        // `powf` and the damping's `tan` run once per block, and the twelve
        // delay lengths move by one add per sample instead of a recompute. The
        // `next` calls in the loop still advance every smoother.
        let size = (self.params.value(self.index.size) * 0.01).clamp(0.2, 1.5);
        let decay = self.params.value(self.index.decay);
        let damp = (self.params.value(self.index.damp) * 0.01).clamp(0.0, 0.95);
        let longest =
            (COMB_BASE[3] * (self.sample_rate / BASE_SR) * size).max(DelayLine::MIN_DELAY);
        let feedback_target = feedback_for_rt60(longest, decay, self.sample_rate);
        let damp_hz = 200.0 + (1.0 - damp) * 12_000.0;
        let damp_target = OnePoleTpt::coefficient_for(damp_hz, self.sample_rate);
        if self.primed {
            self.left.glide_size(self.sample_rate, 0.0, size, frames);
            self.right
                .glide_size(self.sample_rate, STEREO_SPREAD, size, frames);
            let inv = 1.0 / frames as f32;
            self.feedback_step = (feedback_target - self.feedback) * inv;
            self.damp_step = (damp_target - self.damp) * inv;
        } else {
            self.left.set_size(self.sample_rate, 0.0, size);
            self.right.set_size(self.sample_rate, STEREO_SPREAD, size);
            self.feedback = feedback_target;
            self.feedback_step = 0.0;
            self.damp = damp_target;
            self.damp_step = 0.0;
            self.primed = true;
        }

        let [left, right] = ctx.buffers.as_mut_slices();
        for frame in 0..frames {
            let mix = self.params.next(self.index.mix) * 0.01;
            self.params.next(self.index.decay);
            self.params.next(self.index.size);
            self.params.next(self.index.damp);
            let pre_ms = self.params.next(self.index.predelay);
            let width = self.params.next(self.index.width) * 0.01;

            self.feedback += self.feedback_step;
            self.damp += self.damp_step;

            let dry_l = left[frame];
            let dry_r = right[frame];
            let pre_delay = (pre_ms * 0.001 * self.sample_rate).max(DelayLine::MIN_DELAY);
            let pred_l = self.pre_l.read(pre_delay);
            let pred_r = self.pre_r.read(pre_delay);
            self.pre_l.write(dry_l);
            self.pre_r.write(dry_r);

            let mut wet_l = self.left.process(pred_l, self.feedback, self.damp);
            let mut wet_r = self.right.process(pred_r, self.feedback, self.damp);
            let mid = 0.5 * (wet_l + wet_r);
            let side = 0.5 * (wet_l - wet_r) * width;
            wet_l = mid + side;
            wet_r = mid - side;

            left[frame] = dry_l * (1.0 - mix) + wet_l * mix;
            right[frame] = dry_r * (1.0 - mix) + wet_r * mix;
        }
    }
}

impl RtNode for ReverbNode {
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

#[derive(Debug, Default)]
pub struct ReverbFactory;

impl ReverbFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for ReverbFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for ReverbFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(ReverbNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{render_mono, render_stereo, rms_mono, sine, Harness};

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&ReverbFactory::new(), SR, 512)
    }

    #[test]
    fn mix_zero_is_the_dry_path() {
        let mut h = harness();
        h.set(MIX_PARAM, 0.0);
        let _ = render_mono(&mut h, &vec![0.0; 2_048]);
        let input: Vec<[f32; 2]> = (0..2_048)
            .map(|n| {
                let x = (n as f32 * 0.02).sin() * 0.4;
                [x, -x * 0.5]
            })
            .collect();
        assert_eq!(render_stereo(&mut h, &input), input);
    }

    #[test]
    fn a_longer_decay_leaves_more_tail() {
        let burst = sine(440.0, 2_048, SR, 0.5);
        let silence = vec![0.0_f32; 12_000];
        let mut input = burst;
        input.extend_from_slice(&silence);

        let mut short = harness();
        short.set(MIX_PARAM, 100.0);
        short.set("decay", 0.4);
        short.set("predelay", 0.0);
        let out_short = render_mono(&mut short, &input);

        let mut long = harness();
        long.set(MIX_PARAM, 100.0);
        long.set("decay", 6.0);
        long.set("predelay", 0.0);
        let out_long = render_mono(&mut long, &input);

        let tail_s = rms_mono(&out_short[8_000..]);
        let tail_l = rms_mono(&out_long[8_000..]);
        assert!(
            tail_l > tail_s * 2.0,
            "long decay tail {tail_l} should outlast short {tail_s}"
        );
    }
}
