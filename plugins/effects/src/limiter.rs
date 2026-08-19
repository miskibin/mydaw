//! `effects.limiter` -- a stereo-linked peak limiter with short lookahead.
//!
//! ```text
//!   in → input gain → peak detect ──┐
//!                    delay (lookahead) × envelope → ceiling → out
//! ```
//!
//! Instant attack on the envelope, release as a 10–90 % one-pole, stereo linked
//! so a hard-panned transient cannot pull one side of the image. Lookahead is a
//! few milliseconds of delay on the audio path so the gain is already down when
//! the peak arrives — the difference between a limiter and a clipper.
//!
//! Fresh and quiet it is bit-transparent: ceiling at 0 dBFS and no input gain
//! leaves any signal below full scale alone.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, ReadoutDef, RtNode,
    RtNodeFactory, SmoothedParams,
};

use crate::dsp::{linear_to_db, usable_sample_rate, DelayLine};
use crate::effect::{index_of, Bypass, CachedDb, BYPASS_PARAM};

pub const NODE_TYPE_ID: &str = "effects.limiter";
pub const DISPLAY_NAME: &str = "Limiter";
pub const DESCRIPTION: &str =
    "Stereo-linked peak limiter with lookahead. Ceiling is the loudest sample that leaves.";

const SILENCE: f32 = 1.0e-9;
const MAX_LOOKAHEAD_MS: f32 = 5.0;

pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::new("gain", "Gain", "dB", -12.0, 24.0, 0.0, ParamCurve::Db),
    ParamDef::new("ceiling", "Ceiling", "dB", -12.0, 0.0, 0.0, ParamCurve::Db),
    ParamDef::new(
        "release",
        "Release",
        "ms",
        20.0,
        500.0,
        100.0,
        ParamCurve::Log,
    ),
    ParamDef::new(
        "lookahead",
        "Lookahead",
        "ms",
        0.1,
        MAX_LOOKAHEAD_MS,
        1.0,
        ParamCurve::Log,
    )
    .smoothed(40.0),
];

pub static READOUTS: &[ReadoutDef] = &[
    ReadoutDef::new("gr", "Gain Reduction", "dB"),
    ReadoutDef::new("gr_max", "Max Gain Reduction", "dB"),
];

const READOUT_GR: usize = 0;
const READOUT_GR_MAX: usize = 1;

#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    gain: usize,
    ceiling: usize,
    release: usize,
    lookahead: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        Self {
            bypass: index_of(block, BYPASS_PARAM),
            gain: index_of(block, "gain"),
            ceiling: index_of(block, "ceiling"),
            release: index_of(block, "release"),
            lookahead: index_of(block, "lookahead"),
        }
    }
}

/// 10 % → 90 % one-pole coefficient, same convention as the compressor.
fn release_coeff(ms: f32, sample_rate: f32) -> f32 {
    let frames = (ms.max(0.05) * 0.001 * sample_rate).max(1.0);
    (-std::f32::consts::LN_10 * 0.954_242_5 / frames).exp()
}

/// Sliding-window minimum of the last `window` pushed values: a monotonic
/// queue in a fixed ring. **RT-safe** -- `push` is amortised O(1) and never
/// allocates.
///
/// The lookahead contract needs it: the gain applied when a sample leaves the
/// delay must be at most the smallest `needed` gain seen over that sample's
/// whole trip through it, and this answers "smallest over the window" without
/// rescanning the window.
struct MinWindow {
    /// `(sample index, value)`, values strictly increasing from head to back.
    ring: Box<[(u64, f32)]>,
    head: usize,
    len: usize,
    next_index: u64,
}

impl MinWindow {
    /// A window spanning up to `capacity` samples. **Non-RT**: allocates.
    fn new(capacity: usize) -> Self {
        Self {
            ring: vec![(0, 1.0); capacity.max(1)].into_boxed_slice(),
            head: 0,
            len: 0,
            next_index: 0,
        }
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.next_index = 0;
    }

    /// Push `value` and return the minimum of the last `window` values.
    fn push(&mut self, value: f32, window: usize) -> f32 {
        let cap = self.ring.len();
        let index = self.next_index;
        self.next_index += 1;
        // Entries the new value undercuts can never be the minimum again.
        while self.len > 0 {
            let back = (self.head + self.len - 1) % cap;
            if self.ring[back].1 >= value {
                self.len -= 1;
            } else {
                break;
            }
        }
        if self.len == cap {
            // A window longer than the ring cannot happen -- both are sized
            // from MAX_LOOKAHEAD_MS -- but dropping the head is the safe
            // degradation if it ever does.
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.ring[(self.head + self.len) % cap] = (index, value);
        self.len += 1;
        // Expire entries that fell out of the window. The one just pushed
        // never does: its index is `index`, which is inside by definition.
        let cutoff = index.saturating_sub(window.saturating_sub(1) as u64);
        while self.ring[self.head].0 < cutoff {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.ring[self.head].1
    }
}

pub struct LimiterNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    gain: CachedDb,
    ceiling: CachedDb,
    delay_l: DelayLine,
    delay_r: DelayLine,
    /// Smallest `needed` gain over each sample's lookahead span.
    window: MinWindow,
    envelope: f32,
    sample_rate: f32,
}

impl LimiterNode {
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let smoothed = SmoothedParams::new(Arc::clone(&params), sample_rate);
        let index = Indices::new(&params);
        let sr = usable_sample_rate(sample_rate);
        let capacity = ((MAX_LOOKAHEAD_MS * 0.001 * sr) as usize).max(8) + 8;
        Self {
            gain: CachedDb::new(index.gain, &smoothed),
            ceiling: CachedDb::new(index.ceiling, &smoothed),
            index,
            params: smoothed,
            bypass: Bypass::new(max_block, sample_rate),
            delay_l: DelayLine::new(capacity),
            delay_r: DelayLine::new(capacity),
            window: MinWindow::new(capacity),
            envelope: 1.0,
            sample_rate: sr,
        }
    }

    fn reset(&mut self) {
        self.delay_l.clear();
        self.delay_r.clear();
        self.window.clear();
        self.envelope = 1.0;
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        let rel_ms = self.params.value(self.index.release);
        let coeff = release_coeff(rel_ms, self.sample_rate);

        let mut peak_gr = 0.0_f32;
        let mut last_gr = 0.0_f32;
        let [left, right] = ctx.buffers.as_mut_slices();
        for frame in 0..frames {
            let input_gain = self.gain.next(&mut self.params);
            let ceiling = self.ceiling.next(&mut self.params).max(SILENCE);
            self.params.next(self.index.release);
            let look_ms = self.params.next(self.index.lookahead);
            let delay_samples = (look_ms * 0.001 * self.sample_rate).max(DelayLine::MIN_DELAY);

            let in_l = left[frame] * input_gain;
            let in_r = right[frame] * input_gain;
            let delayed_l = self.delay_l.read(delay_samples);
            let delayed_r = self.delay_r.read(delay_samples);
            self.delay_l.write(in_l);
            self.delay_r.write(in_r);

            let peak = in_l.abs().max(in_r.abs()).max(SILENCE);
            let needed = (ceiling / peak).min(1.0);
            // The envelope may not start releasing while the peak is still in
            // the delay: attack instantly to -- and release toward -- the
            // smallest `needed` over the sample now leaving, not the current
            // input's.
            let span = delay_samples.ceil() as usize + 1;
            let floor = self.window.push(needed, span);
            if floor < self.envelope {
                self.envelope = floor;
            } else {
                self.envelope += (floor - self.envelope) * (1.0 - coeff);
            }
            if self.envelope.abs() < 1.0e-8 {
                self.envelope = 0.0;
            }

            left[frame] = delayed_l * self.envelope;
            right[frame] = delayed_r * self.envelope;
            let gr = -linear_to_db(self.envelope.max(SILENCE));
            last_gr = gr;
            peak_gr = peak_gr.max(gr);
        }
        let block = self.params.block();
        block.set_readout(READOUT_GR, last_gr);
        block.set_readout(READOUT_GR_MAX, peak_gr);
    }
}

impl RtNode for LimiterNode {
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
            let block = self.params.block();
            block.set_readout(READOUT_GR, 0.0);
            block.set_readout(READOUT_GR_MAX, 0.0);
        }
        self.bypass.blend(ctx, state, bypassed);
    }
}

#[derive(Debug, Default)]
pub struct LimiterFactory;

impl LimiterFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for LimiterFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for LimiterFactory {
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
        Box::new(LimiterNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{db, peak_mono, render_mono, render_stereo, sine, Harness};

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&LimiterFactory::new(), SR, 512)
    }

    #[test]
    fn a_fresh_limiter_is_a_delayed_copy_below_full_scale() {
        // Lookahead is a delay. The first `lookahead` samples are silence from
        // an empty line; after that a quiet signal is the input, bit for bit,
        // because the envelope never leaves 1.
        let mut h = harness();
        let input: Vec<[f32; 2]> = (0..2_048)
            .map(|n| {
                let x = (n as f32 * 0.03).sin() * 0.05;
                [x, -x]
            })
            .collect();
        let out = render_stereo(&mut h, &input);
        let latency = (0.001 * SR).round() as usize;
        let worst = input
            .iter()
            .zip(out.iter().skip(latency))
            .map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            worst < 1.0e-5,
            "quiet signal should pass the limiter delayed, off by {worst}"
        );
    }

    #[test]
    fn a_hot_signal_does_not_cross_the_ceiling() {
        let mut h = harness();
        h.set("ceiling", -6.0);
        h.set("gain", 0.0);
        h.set("lookahead", 2.0);
        h.set("release", 50.0);
        let input = sine(1_000.0, 48_000, SR, 1.0);
        let out = render_mono(&mut h, &input);
        let peak = db(peak_mono(&out[8_000..]));
        assert!(peak <= -5.9, "ceiling -6 dB, measured peak {peak} dB");
    }

    #[test]
    fn an_isolated_impulse_leaves_at_the_ceiling_not_above_it() {
        // The lookahead contract: the envelope may not start releasing while
        // the peak is still inside the delay. Without the windowed minimum an
        // impulse 12 dB over the ceiling left at roughly ceiling +7 dB.
        let mut h = harness();
        h.set("ceiling", -12.0);
        h.set("lookahead", 2.0);
        h.set("release", 100.0);
        let mut input = vec![0.0_f32; 16_384];
        input[8_192] = 1.0; // an isolated peak 12 dB over the ceiling
        let out = render_mono(&mut h, &input);
        let peak = db(peak_mono(&out[8_192..]));
        assert!(
            peak <= -11.9,
            "ceiling -12 dB, the impulse left at {peak} dB"
        );
        // And it does leave -- delayed and limited, not erased.
        assert!(
            peak >= -12.5,
            "the impulse should still arrive, got {peak} dB"
        );
    }

    #[test]
    fn gain_reduction_is_published_when_it_is_working() {
        let mut h = harness();
        h.set("ceiling", -12.0);
        let input = sine(1_000.0, 4_096, SR, 1.0);
        let _ = render_mono(&mut h, &input);
        let gr = h.readout("gr_max");
        assert!(gr > 6.0, "expected several dB of GR, got {gr}");
    }
}
