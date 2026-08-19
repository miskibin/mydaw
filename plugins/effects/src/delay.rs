//! `effects.delay` -- a tempo-synced stereo delay with damping and ducking.
//!
//! ```text
//!            ┌──────────────── feedback loop ────────────────┐
//!   in ─┬───▶│ delay line ▸ HP ▸ LP ▸ soft clip ▸ × feedback │─┐
//!       │    └───────────────────────────────────────────────┘ │
//!       │                          ▲───────────────────────────┘
//!       └──────────────────────── dry ─────────────▶ mix ─▶ out
//!                                   duck ◀── envelope of in
//! ```
//!
//! # Sync is on by default, and the division is two controls
//! Every vendor splits it -- EchoBoy's `TIME | NOTE | DOT | TRIP` row, Ableton's
//! "Notes, Triplet, Dotted", Comeback Kid's "Straight, Dotted, Triplet, or Free"
//! -- because `[base] × [straight|dotted|triplet]` gives 21 values from two small
//! controls rather than a 21-item menu. The pain point sync fixes is real and
//! specific: "setting the timing in ms…can be quite hard".
//!
//! # The filters are *inside* the loop, and that is the whole feature
//! EchoBoy's Style Editor charges for the difference between "Gain" (the tone of
//! the first repeat) and "Decay" (how the tone changes per repeat). Putting the
//! two one-poles inside the feedback path gives the Decay behaviour for nothing:
//! each pass through the loop filters again, so repeats get darker and thinner
//! the way a tape generation does. Post-loop filtering would only give the first.
//!
//! Six dB/octave and not steeper, deliberately: a resonant filter inside a
//! feedback loop compounds its own ringing every pass.
//!
//! # Feedback goes past unity, and is soft-clipped so that it can
//! Self-oscillation is a documented feature of the units people love -- EchoBoy
//! "allows for the kind of self-oscillation achievable in units like the RE-201
//! Space Echo" -- and EchoBoy *also* warns that high feedback "can boost the
//! output level significantly" and ships a Style called `Limited`. The
//! counter-example is a delay whose loop simply diverges, "resulting in an
//! extremely clipped signal". So the loop has a soft clipper in it: the level
//! saturates instead of running away, and the runaway becomes a sound rather than
//! a bug.
//!
//! # Ducking is one knob, and it ducks the feedback too
//! Valhalla's design win here was *deleting* controls: their duck gives "echo
//! level tracking of the input signal volume **without threshold/attack/release
//! controls**", and Ableton's threshold-and-release version is the less-praised
//! one. Attenuating the feedback as well as the wet output is what produces the
//! behaviour reviewers describe, where "the delayed signal **blooms** and becomes
//! more apparent between the notes".
//!
//! # A division change crossfades; it does not repitch
//! EchoBoy repitches when you *drag* the time knob and crossfades when you click
//! a different note value, and never asks the user which they meant -- the
//! gesture disambiguates the intent. Clicking 1/8 → dotted 1/8 is an *edit*, and
//! an edit must be silent, so the read pointer crossfades between the old
//! position and the new one over [`CROSSFADE_MS`]. Repitching a continuous free
//! drag is specified in the research document and is a follow-up: the
//! interpolator this already uses is what it needs.
//!
//! # Memory
//! The line is allocated for [`MAX_DELAY_SECONDS`] and never grows. Honouring
//! every division at every tempo the transport allows would mean a dotted whole
//! note at 20 BPM -- eighteen seconds, 6.9 MB a channel pair at 48 kHz, times
//! sixteen tracks. Eight seconds costs 3.1 MB at 48 kHz and 6.1 MB at 96 kHz per
//! instance and covers a dotted whole note down to 45 BPM; below that the longest
//! divisions **halve** until they fit, which keeps them on the grid.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode, RtNodeFactory,
    SmoothedParams,
};

use crate::dsp::{soft_clip, usable_sample_rate, DelayLine, EnvelopeFollower, OnePoleTpt};
use crate::effect::{index_of, Bypass, BYPASS_PARAM, MIX_PARAM};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.delay";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "Delay";

/// One line for a tooltip.
pub const DESCRIPTION: &str =
    "Tempo-synced stereo or ping-pong delay with in-loop damping and one-knob ducking";

/// Longest delay the line is allocated for, in seconds. See the module docs on
/// what this costs and what it buys.
pub const MAX_DELAY_SECONDS: f32 = 8.0;

/// Longest free-mode delay the knob offers, in milliseconds.
pub const MAX_FREE_MS: f32 = 4_000.0;

/// How long the read pointer takes to cross to a new delay time.
///
/// 25 ms: long enough that the two tap positions blend rather than butt
/// together, short enough that a division change feels immediate.
pub const CROSSFADE_MS: f32 = 25.0;

/// Beats in each sync division, straight.
///
/// Index order runs slow to fast so a UI can lay the segmented control out left
/// to right the way a musician reads it.
pub const DIVISION_BEATS: [f64; 7] = [4.0, 2.0, 1.0, 0.5, 0.25, 0.125, 0.0625];

/// Labels for [`DIVISION_BEATS`].
pub const DIVISION_LABELS: [&str; 7] = ["1/1", "1/2", "1/4", "1/8", "1/16", "1/32", "1/64"];

/// Multipliers for the straight / dotted / triplet switch.
pub const MODIFIER_FACTORS: [f64; 3] = [1.0, 1.5, 2.0 / 3.0];

/// Labels for [`MODIFIER_FACTORS`].
pub const MODIFIER_LABELS: [&str; 3] = ["Straight", "Dotted", "Triplet"];

/// Input level at which a fully-turned duck knob silences the repeats entirely.
///
/// -12 dBFS. A knob rather than a threshold means there has to be *some*
/// reference, and this is it: normal programme material fully engages the duck,
/// and the gaps between notes let the echoes back.
pub const DUCK_REFERENCE: f32 = 0.25;

/// How fast the duck closes, in milliseconds. Not user-facing -- deleting these
/// controls was the design win.
pub const DUCK_ATTACK_MS: f32 = 5.0;

/// How fast the duck opens again, in milliseconds.
pub const DUCK_RELEASE_MS: f32 = 200.0;

/// What the delay exposes.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::toggle("sync", "Sync", true),
    ParamDef::choice("division", "Division", 7, 3),
    ParamDef::choice("modifier", "Modifier", 3, 0),
    ParamDef::new(
        "time_ms",
        "Time",
        "ms",
        1.0,
        MAX_FREE_MS,
        375.0,
        ParamCurve::Log,
    ),
    // Past unity is reachable and is meant to be marked on the knob, never
    // arrived at by accident. The loop's soft clipper is what makes it musical.
    ParamDef::new(
        "feedback",
        "Feedback",
        "%",
        0.0,
        110.0,
        35.0,
        ParamCurve::Linear,
    ),
    ParamDef::new(
        "damp_hp",
        "Damp HP",
        "Hz",
        20.0,
        2_000.0,
        150.0,
        ParamCurve::Log,
    ),
    ParamDef::new(
        "damp_lp",
        "Damp LP",
        "Hz",
        200.0,
        20_000.0,
        6_500.0,
        ParamCurve::Log,
    ),
    ParamDef::choice("mode", "Mode", 2, 0),
    ParamDef::new("duck", "Duck", "%", 0.0, 100.0, 0.0, ParamCurve::Linear),
    ParamDef::new(MIX_PARAM, "Mix", "%", 0.0, 100.0, 30.0, ParamCurve::Linear),
];

/// Delay time in samples for a sync setting at `bpm`.
///
/// Public so `tests/delay_dsp.rs` can assert the echo lands where this says it
/// should, rather than where a second copy of the arithmetic says it should.
pub fn sync_delay_samples(division: usize, modifier: usize, bpm: f64, sample_rate: f64) -> f64 {
    let beats = DIVISION_BEATS[division.min(DIVISION_BEATS.len() - 1)]
        * MODIFIER_FACTORS[modifier.min(MODIFIER_FACTORS.len() - 1)];
    let bpm = if bpm.is_finite() && bpm > 0.0 {
        bpm
    } else {
        mydaw_rt_core::DEFAULT_BPM
    };
    beats * 60.0 / bpm * sample_rate
}

/// Halve `samples` until it fits in `capacity`, keeping it on the grid.
///
/// A dotted whole note at 20 BPM is eighteen seconds; the line is eight. Halving
/// lands on 1/2 dotted, then 1/4 dotted -- still musical values, still in time --
/// which is a far better answer than clamping to "eight seconds", a length that
/// corresponds to no division at all.
pub fn fit_to_capacity(samples: f64, capacity: f64) -> f64 {
    let mut samples = samples;
    while samples > capacity && samples > 2.0 {
        samples *= 0.5;
    }
    samples.clamp(f64::from(DelayLine::MIN_DELAY), capacity)
}

/// Parameter indices, resolved once off the audio thread.
#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    sync: usize,
    division: usize,
    modifier: usize,
    time_ms: usize,
    feedback: usize,
    damp_hp: usize,
    damp_lp: usize,
    mode: usize,
    duck: usize,
    mix: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        Self {
            bypass: index_of(block, BYPASS_PARAM),
            sync: index_of(block, "sync"),
            division: index_of(block, "division"),
            modifier: index_of(block, "modifier"),
            time_ms: index_of(block, "time_ms"),
            feedback: index_of(block, "feedback"),
            damp_hp: index_of(block, "damp_hp"),
            damp_lp: index_of(block, "damp_lp"),
            mode: index_of(block, "mode"),
            duck: index_of(block, "duck"),
            mix: index_of(block, MIX_PARAM),
        }
    }
}

/// One channel's line and the two filters in its loop.
struct Tap {
    line: DelayLine,
    high: OnePoleTpt,
    low: OnePoleTpt,
}

impl Tap {
    fn new(frames: usize) -> Self {
        Self {
            line: DelayLine::new(frames),
            high: OnePoleTpt::new(),
            low: OnePoleTpt::new(),
        }
    }

    fn reset(&mut self) {
        self.line.clear();
        self.high.reset();
        self.low.reset();
    }

    /// Filter one sample of the feedback path: highpass, then lowpass.
    #[inline]
    fn damp(&mut self, x: f32) -> f32 {
        self.low.low(self.high.high(x))
    }
}

/// The delay node.
pub struct DelayNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    taps: [Tap; 2],
    duck: EnvelopeFollower,
    sample_rate: f32,
    /// Read position in force, in samples.
    delay: f64,
    /// Read position being crossfaded away from.
    previous_delay: f64,
    /// 1.0 while a crossfade is running, falling to 0.0 as it finishes.
    fade: f32,
    /// Per-sample step of the crossfade.
    fade_step: f32,
    /// Whether [`delay`](Self::delay) has ever been derived from a transport.
    ///
    /// The tempo is only known inside `process`, so the first block has to
    /// *adopt* its read position rather than crossfade to it from a position
    /// that was never real. Without this the first block blends against a
    /// two-sample delay and the dry signal appears in the wet path.
    primed: bool,
    /// Last damping frequencies, so the `tan` is only paid on a change.
    damp_hz: (f32, f32),
}

impl DelayNode {
    /// **Non-RT**: allocates the delay lines.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let index = Indices::new(&params);
        let smoothed = SmoothedParams::new(params, sample_rate);
        let rate = usable_sample_rate(sample_rate);
        let frames = (MAX_DELAY_SECONDS * rate).ceil() as usize;
        let mut node = Self {
            params: smoothed,
            index,
            bypass: Bypass::new(max_block, sample_rate),
            taps: [Tap::new(frames), Tap::new(frames)],
            duck: EnvelopeFollower::new(),
            sample_rate: rate,
            delay: 0.0,
            previous_delay: 0.0,
            fade: 0.0,
            fade_step: 1.0 / (CROSSFADE_MS * 0.001 * rate).max(1.0),
            primed: false,
            damp_hz: (f32::NAN, f32::NAN),
        };
        node.duck
            .set_times(DUCK_ATTACK_MS, DUCK_RELEASE_MS, node.sample_rate);
        node
    }

    fn reset(&mut self) {
        for tap in &mut self.taps {
            tap.reset();
        }
        self.duck.reset();
        self.fade = 0.0;
        self.primed = false;
    }

    /// Longest delay the allocated line can serve, in samples.
    fn capacity(&self) -> f64 {
        self.taps[0].line.capacity() as f64
    }

    /// The delay this block should be reading at, in samples.
    fn target_delay(&self, transport_bpm: f64) -> f64 {
        let synced = self.params.choice(self.index.sync) != 0;
        let raw = if synced {
            sync_delay_samples(
                self.params.choice(self.index.division),
                self.params.choice(self.index.modifier),
                transport_bpm,
                f64::from(self.sample_rate),
            )
        } else {
            f64::from(self.params.goal(self.index.time_ms)) * 0.001 * f64::from(self.sample_rate)
        };
        fit_to_capacity(raw, self.capacity())
    }

    /// Recompute the loop filters, only when their frequencies have moved.
    fn refresh_damping(&mut self) {
        let hp = self.params.value(self.index.damp_hp);
        let lp = self.params.value(self.index.damp_lp);
        if (hp, lp) == self.damp_hz {
            return;
        }
        self.damp_hz = (hp, lp);
        for tap in &mut self.taps {
            tap.high.set_cutoff(hp, self.sample_rate);
            tap.low.set_cutoff(lp, self.sample_rate);
        }
    }

    /// Read a tap at the current position, crossfading if a change is in flight.
    #[inline]
    fn read(&self, channel: usize) -> f32 {
        let line = &self.taps[channel].line;
        let fresh = line.read(self.delay as f32);
        if self.fade <= 0.0 {
            fresh
        } else {
            let stale = line.read(self.previous_delay as f32);
            fresh * (1.0 - self.fade) + stale * self.fade
        }
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        self.refresh_damping();

        // A division change -- or a tempo change, which is the same thing to the
        // delay -- restarts the crossfade. An edit must be silent.
        let target = self.target_delay(ctx.transport.bpm);
        if !self.primed {
            self.primed = true;
            self.delay = target;
            self.previous_delay = target;
            self.fade = 0.0;
        } else if (target - self.delay).abs() > 0.5 {
            self.previous_delay = self.delay;
            self.delay = target;
            // A change arriving mid-fade restarts from the position we were
            // heading to, rather than trying to blend three taps.
            self.fade = 1.0;
        } else {
            self.delay = target;
        }

        let ping_pong = self.params.choice(self.index.mode) == 1;

        let channels = ctx.buffers.as_mut_slices();
        let [left, right] = channels;
        for frame in 0..frames {
            let feedback = self.params.next(self.index.feedback) * 0.01;
            let mix = self.params.next(self.index.mix) * 0.01;
            let duck_depth = self.params.next(self.index.duck) * 0.01;
            // Keep the block-rate parameters walking so "settled" stays honest.
            self.params.next(self.index.damp_hp);
            self.params.next(self.index.damp_lp);
            self.params.next(self.index.time_ms);

            let dry_l = left[frame];
            let dry_r = right[frame];

            // One duck envelope for both channels: a stereo image that ducked
            // per side would wander.
            let envelope = self.duck.process(dry_l.abs().max(dry_r.abs()));
            let duck = if duck_depth > 0.0 {
                1.0 - duck_depth * (envelope / DUCK_REFERENCE).min(1.0)
            } else {
                1.0
            };

            let wet_l = self.read(0);
            let wet_r = self.read(1);

            // The loop: damp, saturate, scale. The clipper is what makes
            // feedback past 100 % a sound rather than a divergence.
            let fed_l = soft_clip(self.taps[0].damp(wet_l) * feedback * duck);
            let fed_r = soft_clip(self.taps[1].damp(wet_r) * feedback * duck);

            if ping_pong {
                // Input summed to mono and cross-fed, which is what ping-pong
                // is: EchoBoy's is "always stereo out, but the input is always
                // summed to Mono", and FabFilter's applies cross feedback only.
                let mono = (dry_l + dry_r) * 0.5;
                self.taps[0].line.write(mono + fed_r);
                self.taps[1].line.write(fed_l);
            } else {
                self.taps[0].line.write(dry_l + fed_l);
                self.taps[1].line.write(dry_r + fed_r);
            }

            left[frame] = dry_l * (1.0 - mix) + wet_l * duck * mix;
            right[frame] = dry_r * (1.0 - mix) + wet_r * duck * mix;

            if self.fade > 0.0 {
                self.fade = (self.fade - self.fade_step).max(0.0);
            }
        }
    }
}

impl RtNode for DelayNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        self.params.begin_block();
        let bypassed = self.params.choice(self.index.bypass) != 0;
        let state = self.bypass.begin(ctx, bypassed);

        if state.processes() {
            if state.resets() {
                // Coming back from a full bypass, the position is re-derived on
                // the next block rather than crossfaded from a stale one.
                self.reset();
            }
            self.render(ctx);
        } else {
            self.params.skip_all(ctx.frames);
        }

        self.bypass.blend(ctx, state, bypassed);
    }
}

/// Builds [`DelayNode`]s.
#[derive(Debug, Default)]
pub struct DelayFactory;

impl DelayFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for DelayFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for DelayFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(DelayNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        db, level_db_at, peak_mono, render_mono, render_stereo, rms_mono, silence, sine, Harness,
    };

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&DelayFactory::new(), SR, 512)
    }

    /// Let every parameter ramp finish before the measurement starts.
    ///
    /// Mix defaults to 30 %, so a test that turns it to 100 % and immediately
    /// sends a click would measure the *dry* click leaking through the ramp
    /// rather than the echo. Ten milliseconds of silence costs nothing and
    /// removes a whole class of false failure.
    fn settle(h: &mut Harness) {
        render_mono(h, &silence(2_048));
    }

    /// A click at frame 0 followed by silence.
    fn click(frames: usize) -> Vec<f32> {
        let mut signal = silence(frames);
        signal[0] = 1.0;
        signal
    }

    /// Frames at which the output rises above `floor`, with a refractory gap so
    /// one echo counts once.
    fn onsets(samples: &[f32], floor: f32) -> Vec<usize> {
        let mut hits = Vec::new();
        let mut last = None;
        for (n, s) in samples.iter().enumerate() {
            if s.abs() > floor && last.is_none_or(|previous| n > previous + 64) {
                hits.push(n);
                last = Some(n);
            }
        }
        hits
    }

    #[test]
    fn a_fresh_delay_starts_silent_and_leaves_the_dry_signal_where_it_was() {
        // Mix defaults to 30 %, so a fresh delay is *not* transparent -- but its
        // line is empty, so the first pass through is the dry signal at 70 %
        // with nothing added.
        let mut h = harness();
        let input = vec![[0.5_f32, 0.5]; 128];
        let out = render_stereo(&mut h, &input);
        assert!((out[64][0] - 0.35).abs() < 1e-5, "{}", out[64][0]);
    }

    #[test]
    fn a_synced_echo_lands_on_the_grid_the_division_names() {
        let mut h = harness().at_bpm(120.0);
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 0.0);
        h.set("division", 3.0); // 1/8
        h.set("modifier", 0.0); // straight
        settle(&mut h);

        let out = render_mono(&mut h, &click(48_000));
        let hits = onsets(&out, 0.05);
        // 1/8 at 120 BPM is 0.25 s = 12000 samples.
        let expected = sync_delay_samples(3, 0, 120.0, SR);
        assert!((expected - 12_000.0).abs() < 1.0, "{expected}");
        assert!(!hits.is_empty(), "no echo at all");
        assert!(
            (hits[0] as f64 - expected).abs() < 4.0,
            "the echo landed at {} rather than {expected}",
            hits[0]
        );
    }

    #[test]
    fn dotted_and_triplet_are_exactly_the_ratios_they_claim() {
        let straight = sync_delay_samples(3, 0, 120.0, SR);
        let dotted = sync_delay_samples(3, 1, 120.0, SR);
        let triplet = sync_delay_samples(3, 2, 120.0, SR);
        assert!((dotted / straight - 1.5).abs() < 1e-9);
        assert!((triplet / straight - 2.0 / 3.0).abs() < 1e-9);

        // And the one the research calls the single most-cited musical value:
        // dotted 1/8 at 120 BPM is 375 ms.
        assert!((dotted / SR - 0.375).abs() < 1e-9, "{}", dotted / SR);
    }

    #[test]
    fn a_dotted_eighth_echo_really_arrives_at_the_dotted_eighth() {
        let mut h = harness().at_bpm(120.0);
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 0.0);
        h.set("division", 3.0);
        h.set("modifier", 1.0);
        settle(&mut h);
        let out = render_mono(&mut h, &click(48_000));
        let hits = onsets(&out, 0.05);
        assert!(!hits.is_empty());
        assert!(
            (hits[0] as f64 - 18_000.0).abs() < 4.0,
            "dotted 1/8 at 120 BPM should be 18000 samples, got {}",
            hits[0]
        );
    }

    #[test]
    fn the_tempo_the_transport_reports_is_the_tempo_the_delay_uses() {
        for bpm in [90.0_f64, 130.0, 174.0] {
            let mut h = harness().at_bpm(bpm);
            h.set(MIX_PARAM, 100.0);
            h.set("feedback", 0.0);
            h.set("division", 2.0); // 1/4
            settle(&mut h);
            let out = render_mono(&mut h, &click(96_000));
            let hits = onsets(&out, 0.05);
            let expected = 60.0 / bpm * SR;
            assert!(!hits.is_empty(), "no echo at {bpm} BPM");
            assert!(
                (hits[0] as f64 - expected).abs() < 6.0,
                "{bpm} BPM: echo at {} rather than {expected}",
                hits[0]
            );
        }
    }

    #[test]
    fn feedback_makes_a_decaying_series_of_echoes_at_the_rate_it_says() {
        let mut h = harness().at_bpm(120.0);
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 50.0);
        h.set("division", 4.0); // 1/16 = 6000 samples
                                // Wide open filters, or the decay measured would be the damping's.
        h.set("damp_hp", 20.0);
        h.set("damp_lp", 20_000.0);
        settle(&mut h);

        let out = render_mono(&mut h, &click(96_000));
        let hits = onsets(&out, 0.005);
        assert!(hits.len() >= 4, "only {} echoes", hits.len());

        // Energy per repeat, not peak: a click through two one-poles comes out
        // as a smeared burst whose *peak* depends on where the smear lands, and
        // the whole window between two echoes is what "this repeat" means.
        let step = 6_000; // 1/16 at 120 BPM
        let repeat = |k: usize| rms_mono(&out[k * step..(k + 1) * step]);
        let first = db(repeat(2) / repeat(1));
        let second = db(repeat(3) / repeat(2));
        // Successive repeats fall by a *constant* ratio -- that is what a
        // feedback loop does, and it is the claim worth testing.
        assert!(
            (first - second).abs() < 1.0,
            "the decay is not constant: {first:.2} dB then {second:.2} dB"
        );
        // A little under the 6 dB the knob implies, because the loop also holds
        // two one-poles and a soft clipper -- which is the per-pass darkening
        // the next test is about.
        assert!(
            first > -11.0 && first < -4.5,
            "50 % feedback decayed {first:.2} dB a repeat"
        );
    }

    #[test]
    fn feedback_past_unity_saturates_instead_of_diverging() {
        // The property that makes 110 % a feature rather than a bug report.
        let mut h = harness().at_bpm(160.0);
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 110.0);
        h.set("division", 5.0);
        h.set("damp_hp", 20.0);
        h.set("damp_lp", 20_000.0);

        let mut signal = sine(220.0, 4_800, SR, 0.8);
        signal.extend(silence(240_000)); // five seconds of self-oscillation
        let out = render_mono(&mut h, &signal);
        let tail = peak_mono(&out[200_000..]);
        assert!(tail > 0.01, "it should still be ringing, got {tail}");
        assert!(
            tail <= 1.05,
            "the loop diverged to {tail}; the soft clipper is not clipping"
        );
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn the_damping_filters_are_in_the_loop_so_repeats_get_darker_each_pass() {
        // The tape-generation behaviour, measured: the *fourth* repeat has to be
        // darker than the first, which only happens if the filter is inside.
        let mut h = harness().at_bpm(120.0);
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 70.0);
        h.set("division", 4.0); // 6000 samples
        h.set("damp_lp", 2_000.0);
        h.set("damp_hp", 20.0);

        let mut signal = silence(96_000);
        // A short burst with energy well above and below the damping corner.
        for (n, s) in signal.iter_mut().enumerate().take(512) {
            let t = n as f64 / SR;
            *s = 0.4
                * ((std::f64::consts::TAU * 500.0 * t).sin()
                    + (std::f64::consts::TAU * 6_000.0 * t).sin()) as f32;
        }
        let out = render_mono(&mut h, &signal);

        let brightness = |start: usize| {
            let window = &out[start..start + 4_096];
            level_db_at(window, 6_000.0, SR) - level_db_at(window, 500.0, SR)
        };
        let first = brightness(6_000);
        let fourth = brightness(24_000);
        assert!(
            fourth < first - 6.0,
            "repeat 1 was {first:.1} dB bright and repeat 4 was {fourth:.1} dB; \
             the filter is not compounding"
        );
    }

    #[test]
    fn ping_pong_alternates_channels_and_stereo_does_not() {
        let mut h = harness().at_bpm(120.0);
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 60.0);
        h.set("division", 4.0);
        h.set("mode", 1.0);
        h.set("damp_hp", 20.0);
        h.set("damp_lp", 20_000.0);
        settle(&mut h);

        let mut input = vec![[0.0_f32; 2]; 96_000];
        input[0] = [1.0, 1.0];
        let out = render_stereo(&mut h, &input);

        let window = |at: usize, ch: usize| {
            out[at..at + 512]
                .iter()
                .fold(0.0_f32, |peak, f| peak.max(f[ch].abs()))
        };
        // Echo 1 in the left, echo 2 in the right.
        assert!(
            window(6_000, 0) > 10.0 * window(6_000, 1),
            "first echo is not left-only"
        );
        assert!(
            window(12_000, 1) > 10.0 * window(12_000, 0),
            "second is not right-only"
        );
    }

    #[test]
    fn ducking_pulls_the_repeats_down_while_the_source_plays_and_lets_them_back() {
        // A 50 % blend, so the dry signal is in the output and the ducking is
        // measurable as "the repeats went away and the source did not".
        let ducked = |duck: f32| {
            let mut h = harness().at_bpm(120.0);
            h.set(MIX_PARAM, 50.0);
            h.set("feedback", 60.0);
            h.set("division", 3.0);
            h.set("duck", duck);
            h.set("damp_hp", 20.0);
            h.set("damp_lp", 20_000.0);
            // Two seconds of sustained source, then a gap for the tail.
            let mut signal = sine(440.0, 96_000, SR, 0.6);
            signal.extend(silence(48_000));
            let out = render_mono(&mut h, &signal);
            (
                rms_mono(&out[60_000..90_000]),
                rms_mono(&out[100_000..120_000]),
            )
        };

        let mut signal = sine(440.0, 96_000, SR, 0.6);
        signal.extend(silence(48_000));
        let (during, _) = ducked(100.0);
        let dry = rms_mono(&signal[60_000..90_000]) * 0.5;
        assert!(
            db(during / dry) < 1.0,
            "the repeats were not ducked: {:.2} dB above the dry half",
            db(during / dry)
        );
        // In the gap the echoes bloom back -- which is the behaviour reviewers
        // describe, and the reason ducking the feedback matters as well as the
        // output. Measured against the *ducked* wet level, because there is no
        // dry signal left to measure against.
        let (_, after) = ducked(100.0);
        println!(
            "duck: dry half {dry:.4}, ducked while playing {during:.4}, blooming in the gap {after:.4}"
        );
        assert!(after > 0.02, "nothing bloomed back in the gap: {after}");

        // Without ducking the same passage is louder, or the knob does nothing.
        let (open, _) = ducked(0.0);
        assert!(
            open > during * 1.2,
            "ducking made no difference: {open:.4} against {during:.4}"
        );
    }

    #[test]
    fn changing_the_division_crossfades_rather_than_clicking() {
        // An edit gesture must be silent. A steady tone through a 100 % wet
        // delay means any discontinuity in the read pointer is a discontinuity
        // in the output.
        let mut h = harness().at_bpm(120.0);
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 40.0);
        h.set("division", 3.0);
        let signal = sine(440.0, 96_000, SR, 0.5);
        render_mono(&mut h, &signal);

        h.set("division", 4.0);
        let out = render_mono(&mut h, &signal);
        let worst = out
            .windows(2)
            .map(|p| (p[1] - p[0]).abs())
            .fold(0.0_f32, f32::max);
        // A 440 Hz sine at amplitude ~1 moves by at most 2*pi*440/48000 = 0.058
        // per sample on its own.
        assert!(
            worst < 0.12,
            "the division change produced a jump of {worst}"
        );
    }

    #[test]
    fn a_division_too_long_for_the_line_halves_until_it_fits_and_stays_on_the_grid() {
        let capacity = f64::from(MAX_DELAY_SECONDS) * SR;
        // A dotted whole note at 20 BPM is eighteen seconds.
        let asked = sync_delay_samples(0, 1, 20.0, SR);
        assert!(asked / SR > 17.0, "{}", asked / SR);
        let fitted = fit_to_capacity(asked, capacity);
        assert!(fitted <= capacity);
        // Still a musical value: exactly a quarter of what was asked for.
        assert!((asked / fitted - 4.0).abs() < 1e-9, "{}", asked / fitted);

        // And anything from 45 BPM up fits untouched.
        let at_45 = sync_delay_samples(0, 1, 45.0, SR);
        assert_eq!(fit_to_capacity(at_45, capacity), at_45);
    }

    #[test]
    fn free_mode_uses_milliseconds_and_ignores_the_tempo() {
        let mut h = harness().at_bpm(90.0);
        h.set("sync", 0.0);
        h.set("time_ms", 200.0);
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 0.0);
        settle(&mut h);
        let out = render_mono(&mut h, &click(48_000));
        let hits = onsets(&out, 0.05);
        assert!(!hits.is_empty());
        assert!(
            (hits[0] as f64 - 0.2 * SR).abs() < 8.0,
            "200 ms should be 9600 samples, got {}",
            hits[0]
        );
    }

    #[test]
    fn a_bypassed_delay_is_the_input_and_comes_back_with_a_fresh_tail() {
        let mut h = harness().at_bpm(120.0);
        h.set(MIX_PARAM, 100.0);
        h.set("feedback", 80.0);
        let signal = sine(440.0, 48_000, SR, 0.5);
        render_mono(&mut h, &signal);

        h.set("bypass", 1.0);
        let input: Vec<[f32; 2]> = signal.iter().map(|s| [*s, *s]).collect();
        render_stereo(&mut h, &input);
        let out = render_stereo(&mut h, &input);
        assert_eq!(out, input, "a settled bypass is the input, bit for bit");

        // Coming back, the eight seconds of tail that was in the line must not
        // reappear: the first echo is of the *new* audio.
        h.set("bypass", 0.0);
        let out = render_mono(&mut h, &click(24_000));
        assert!(
            peak_mono(&out[..4_000]) <= 1.0,
            "stale audio came back with the delay"
        );
    }
}
