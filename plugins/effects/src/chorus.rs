//! `effects.chorus` -- two or three modulated delay voices per side.
//!
//! ```text
//!            ┌─ voice 1 ─ delay(base + depth·sin(φ        )) ─┐
//!   in ─┬───▶├─ voice 2 ─ delay(base + depth·sin(φ + 1/n  )) ─┼─ ÷√n ─┐
//!       │    └─ voice 3 ─ delay(base + depth·sin(φ + 2/n  )) ─┘       │
//!       └───────────────────────── dry ──────────────────────────── mix ─▶ out
//!            left and right read the same φ a quarter turn apart at full spread
//! ```
//!
//! # What a chorus actually is
//! A copy of the signal delayed by five to twenty milliseconds is heard as a
//! second source rather than an echo (below about 30 ms the ear fuses the two),
//! and *moving* that delay stretches and squeezes the copy's waveform, which is
//! a pitch shift proportional to the rate of change. So the parts are: a delay
//! long enough not to comb the source into a flanger, a modulation slow and
//! shallow enough to read as detune rather than wobble, and more than one voice
//! so the detunings are in different directions at the same instant.
//!
//! # Why the voices share one oscillator
//! Three independent LFOs at the same rate would drift into and out of phase and
//! the effect would breathe with the beat frequency. One phase read at `0`,
//! `1/n` and `2/n` turns keeps the voices permanently spread across the cycle:
//! when one is sharp another is flat, which is what makes the sum sound like an
//! ensemble instead of like one detuned copy.
//!
//! # Level: `1/√n`, not `1/n`
//! The voices are the same source at different delays, so they are largely
//! *uncorrelated* at the modulation depths that matter -- their powers add, not
//! their amplitudes. Dividing by `n` would make three voices audibly quieter
//! than two and turn the voice switch into a volume control; dividing by `√n`
//! keeps the wet level where the mix knob put it.
//!
//! # The base delay and depth glide slowly on purpose
//! Both move the read pointer, and a read pointer that crosses the whole knob
//! range in the standard 10 ms is a pitch transient. They are smoothed over
//! [`SWEEP_MS`] instead, which makes a full-range drag a tape-style sweep rather
//! than a click -- the same reasoning as the delay's crossfade, arriving at a
//! different answer because a chorus's range is 30 ms and not 8 seconds.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode, RtNodeFactory,
    SmoothedParams, CHANNELS,
};

use crate::dsp::{usable_sample_rate, DelayLine, Lfo};
use crate::effect::{index_of, Bypass, BYPASS_PARAM, MIX_PARAM};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.chorus";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "Chorus";

/// One line for a tooltip.
pub const DESCRIPTION: &str =
    "Two or three modulated delay voices with stereo spread -- ensemble, not echo";

/// Longest base delay the knob offers, in milliseconds.
pub const MAX_DELAY_MS: f32 = 30.0;

/// Widest modulation the knob offers, in milliseconds either side of the base.
pub const MAX_DEPTH_MS: f32 = 10.0;

/// How long the base delay and depth take to cross their whole range.
pub const SWEEP_MS: f32 = 150.0;

/// Voices the switch offers.
pub const VOICE_COUNTS: [usize; 2] = [2, 3];

/// What the chorus exposes.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::new("rate", "Rate", "Hz", 0.01, 10.0, 0.6, ParamCurve::Log),
    ParamDef::new(
        "depth",
        "Depth",
        "ms",
        0.0,
        MAX_DEPTH_MS,
        2.5,
        ParamCurve::Linear,
    )
    .smoothed(SWEEP_MS),
    ParamDef::new(
        "delay",
        "Delay",
        "ms",
        1.0,
        MAX_DELAY_MS,
        9.0,
        ParamCurve::Log,
    )
    .smoothed(SWEEP_MS),
    ParamDef::choice("voices", "Voices", 2, 1),
    ParamDef::new(
        "spread",
        "Spread",
        "%",
        0.0,
        100.0,
        50.0,
        ParamCurve::Linear,
    ),
    ParamDef::new(MIX_PARAM, "Mix", "%", 0.0, 100.0, 50.0, ParamCurve::Linear),
];

/// Parameter indices, resolved once off the audio thread.
#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    rate: usize,
    depth: usize,
    delay: usize,
    voices: usize,
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
            voices: index_of(block, "voices"),
            spread: index_of(block, "spread"),
            mix: index_of(block, MIX_PARAM),
        }
    }
}

/// The chorus node.
pub struct ChorusNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    lines: [DelayLine; CHANNELS],
    lfo: Lfo,
    /// Last rate the increment was computed for.
    rate_hz: f32,
    sample_rate: f32,
}

impl ChorusNode {
    /// **Non-RT**: allocates the two delay lines.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let index = Indices::new(&params);
        let rate = usable_sample_rate(sample_rate);
        // The longest read the knobs can ask for, plus a millisecond of margin.
        let frames = (((MAX_DELAY_MS + MAX_DEPTH_MS + 1.0) * 0.001) * rate).ceil() as usize;
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

    /// The sum of one channel's voices, already scaled by `1/√n`.
    #[inline]
    fn voices(&self, channel: usize, base: f32, depth: f32, side: f32, count: usize) -> f32 {
        let mut sum = 0.0;
        for voice in 0..count {
            let offset = side + voice as f32 / count as f32;
            let ms = base + depth * self.lfo.sine_at(offset);
            sum += self.lines[channel].read(ms * 0.001 * self.sample_rate);
        }
        sum / (count as f32).sqrt()
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        let count = VOICE_COUNTS[self.params.choice(self.index.voices).min(1)];
        let [left, right] = ctx.buffers.as_mut_slices();

        for frame in 0..frames {
            let rate = self.params.next(self.index.rate);
            let depth = self.params.next(self.index.depth);
            let base = self.params.next(self.index.delay);
            let spread = self.params.next(self.index.spread) * 0.01;
            let mix = self.params.next(self.index.mix) * 0.01;

            if rate != self.rate_hz {
                self.rate_hz = rate;
                self.lfo.set_rate(rate, self.sample_rate);
            }
            self.lfo.advance();

            let dry_l = left[frame];
            let dry_r = right[frame];

            // A quarter of a cycle between the sides at 100 %, an eighth each
            // way. Not a half: the voices are already spread `1/n` apart, so a
            // half-cycle offset would map one side's set of taps exactly onto
            // the other's and the effect would collapse to mono at the widest
            // setting -- which is the opposite of what the knob says.
            let wet_l = self.voices(0, base, depth, spread * 0.125, count);
            let wet_r = self.voices(1, base, depth, -spread * 0.125, count);

            self.lines[0].write(dry_l);
            self.lines[1].write(dry_r);

            left[frame] = dry_l * (1.0 - mix) + wet_l * mix;
            right[frame] = dry_r * (1.0 - mix) + wet_r * mix;
        }
    }
}

impl RtNode for ChorusNode {
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

/// Builds [`ChorusNode`]s.
#[derive(Debug, Default)]
pub struct ChorusFactory;

impl ChorusFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for ChorusFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for ChorusFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(ChorusNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{peak_mono, render_mono, render_stereo, rms, silence, sine, Harness};

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&ChorusFactory::new(), SR, 512)
    }

    /// A click at frame 0 followed by silence.
    fn click(frames: usize) -> Vec<f32> {
        let mut signal = silence(frames);
        signal[0] = 1.0;
        signal
    }

    /// Frames at which `samples` rises above `floor`, one hit per burst.
    fn onsets(samples: &[f32], floor: f32) -> Vec<usize> {
        let mut hits = Vec::new();
        let mut last: Option<usize> = None;
        for (n, s) in samples.iter().enumerate() {
            if s.abs() > floor && last.is_none_or(|previous| n > previous + 32) {
                hits.push(n);
                last = Some(n);
            }
        }
        hits
    }

    #[test]
    fn a_fresh_chorus_passes_the_dry_signal_while_its_lines_fill() {
        // Mix defaults to 50 %, so a fresh chorus is not transparent -- but for
        // the first few milliseconds its lines are empty and the output is
        // exactly half the input. Anything else would be the wet path inventing
        // audio it has not been given.
        let mut h = harness();
        let input = vec![[0.5_f32, 0.5]; 128];
        let out = render_stereo(&mut h, &input);
        assert!((out[64][0] - 0.25).abs() < 1e-5, "{}", out[64][0]);
    }

    #[test]
    fn three_voices_sit_where_the_shared_oscillator_puts_them() {
        // The structural claim: one phase read at 0, 1/3 and 2/3 of a turn. With
        // the oscillator practically stopped, an impulse comes back as three
        // taps at base + depth·sin(2πk/3) -- 9 ms, 13.33 ms and 4.67 ms.
        let mut h = harness();
        h.set(MIX_PARAM, 100.0);
        h.set("rate", 0.01);
        h.set("depth", 5.0);
        h.set("delay", 9.0);
        h.set("voices", 1.0); // three
        h.set("spread", 0.0);
        render_mono(&mut h, &silence(12_000)); // let the slow sweep settle

        let out = render_mono(&mut h, &click(4_096));
        let hits = onsets(&out, 0.05);
        assert_eq!(hits.len(), 3, "expected three taps, got {hits:?}");
        let ms = |samples: usize| samples as f32 / SR as f32 * 1_000.0;
        let mut times: Vec<f32> = hits.iter().map(|n| ms(*n)).collect();
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let expected = [
            9.0 - 5.0 * (2.0 * std::f32::consts::PI / 3.0).sin(),
            9.0,
            9.0 + 5.0 * (2.0 * std::f32::consts::PI / 3.0).sin(),
        ];
        for (got, want) in times.iter().zip(expected) {
            assert!(
                (got - want).abs() < 0.3,
                "a voice landed at {got:.2} ms rather than {want:.2} ms ({times:?})"
            );
        }
    }

    #[test]
    fn two_voices_are_two_voices() {
        let mut h = harness();
        h.set(MIX_PARAM, 100.0);
        h.set("rate", 0.01);
        h.set("depth", 5.0);
        h.set("delay", 9.0);
        h.set("voices", 0.0); // two
        h.set("spread", 0.0);
        render_mono(&mut h, &silence(12_000));
        let out = render_mono(&mut h, &click(4_096));
        // Offsets 0 and 1/2: sin is zero at both, so the two taps coincide at
        // the base delay and the sum is louder rather than smeared.
        let hits = onsets(&out, 0.05);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!((hits[0] as f32 / SR as f32 * 1_000.0 - 9.0).abs() < 0.3);
        assert!(
            peak_mono(&out) > 0.4,
            "the taps cancelled: {}",
            peak_mono(&out)
        );
    }

    #[test]
    fn the_delay_really_moves_so_the_copy_is_detuned_rather_than_merely_late() {
        // A chorus that did not modulate would be a very short delay. Measured
        // as the thing modulation *is*: the arrival time of the same impulse at
        // two different points in the LFO cycle.
        let arrival = |after_ms: f32| {
            let mut h = harness();
            h.set(MIX_PARAM, 100.0);
            h.set("rate", 1.0);
            h.set("depth", 8.0);
            h.set("delay", 12.0);
            h.set("voices", 0.0);
            h.set("spread", 0.0);
            render_mono(&mut h, &silence(12_000)); // settle the sweeps
            let wait = (after_ms * 0.001 * SR as f32) as usize;
            render_mono(&mut h, &silence(wait));
            let out = render_mono(&mut h, &click(4_096));
            onsets(&out, 0.05).first().copied().unwrap_or(0) as f32 / SR as f32 * 1_000.0
        };
        // A quarter of a 1 Hz cycle apart: sin goes from ~0 to ~1, so the delay
        // should move by most of the 8 ms depth.
        let early = arrival(0.0);
        let late = arrival(250.0);
        assert!(
            (late - early).abs() > 4.0,
            "the delay barely moved: {early:.2} ms then {late:.2} ms"
        );
    }

    #[test]
    fn spread_reads_the_oscillator_at_two_points_so_the_sides_differ() {
        // Zero spread is the mono-safe end -- both sides read the same phase, so
        // a mono source stays mono. Turning it up has to make them genuinely
        // different signals rather than merely a level difference.
        let mono_source: Vec<[f32; 2]> = sine(440.0, 48_000, SR, 0.5)
            .into_iter()
            .map(|s| [s, s])
            .collect();

        let mut h = harness();
        h.set(MIX_PARAM, 100.0);
        h.set("rate", 1.0);
        h.set("depth", 6.0);
        h.set("delay", 12.0);
        h.set("spread", 0.0);
        let out = render_stereo(&mut h, &mono_source);
        // Past the 10 ms it takes the spread knob itself to reach zero.
        assert!(
            out[2_048..].iter().all(|f| (f[0] - f[1]).abs() < 1e-6),
            "at zero spread a mono source must stay mono"
        );

        let mut h = harness();
        h.set(MIX_PARAM, 100.0);
        h.set("rate", 1.0);
        h.set("depth", 6.0);
        h.set("delay", 12.0);
        h.set("spread", 100.0);
        let out = render_stereo(&mut h, &mono_source);
        let difference = out[24_000..]
            .iter()
            .map(|f| (f[0] - f[1]).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            difference > 0.2,
            "a fully spread chorus is still mono: the sides differ by {difference}"
        );
    }

    #[test]
    fn the_wet_path_keeps_the_level_the_dry_one_had() {
        // The 1/√n claim: switching from two voices to three must not change how
        // loud the effect is.
        let level = |voices: f32| {
            let mut h = harness();
            h.set(MIX_PARAM, 100.0);
            h.set("voices", voices);
            h.set("rate", 0.7);
            h.set("depth", 3.0);
            let signal: Vec<[f32; 2]> = sine(220.0, 96_000, SR, 0.5)
                .into_iter()
                .map(|s| [s, s])
                .collect();
            let out = render_stereo(&mut h, &signal);
            rms(&out[48_000..])
        };
        let two = level(0.0);
        let three = level(1.0);
        assert!(
            (20.0 * (three / two).log10()).abs() < 2.0,
            "two voices {two:.3}, three voices {three:.3}"
        );
    }

    #[test]
    fn a_bypassed_chorus_is_the_input_again() {
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
