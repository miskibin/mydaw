//! `effects.saturator` -- three waveshapers, oversampled, with the level
//! matched so an A/B is honest.
//!
//! ```text
//!   in ─┬─▶ tilt ▸ drive ▸ [ ↑4x ▸ shape ▸ DC block ▸ ↓4x ] ▸ auto-gain ▸ out gain ─┐
//!       │                                                                            ├▶ mix ▶ out
//!       └────────────── dry, delayed by the oversampler's latency ───────────────────┘
//! ```
//!
//! # Drive-compensated output, and why it is a table and not a follower
//! The loudest complaint in the whole saturation research is that drive makes
//! things louder and louder sounds better: the KVR thread *"is a saturator
//! without autogain worth it in 2023?"* has its author saying the loudness bump
//! "fool[s] me every time into thinking the new saturated sound is better". Both
//! Decapitator and Saturn ship compensation, and Softube shipped the most-used
//! free saturator in existence *without* it and retrofitted gain staging under
//! user pressure.
//!
//! But 47 % of that same poll would rather trim by hand, and every stated reason
//! -- "It usually doesn't work 100 % accurately", "I deactivate auto-gain
//! immediately in any plugin that provides it" -- describes a **level-tracking**
//! auto-gain behaving like a slow compressor and eating the dynamic payoff.
//! Decapitator's is drive-indexed and deterministic, and nobody complains about
//! that one.
//!
//! So the compensation here is a **static table**, built once in the factory by
//! pushing a fixed deterministic noise sequence through each shaper at each drive
//! step and measuring the RMS change. It is a function of `(type, drive)` and of
//! nothing else: signal-independent, automatable, and bit-reproducible across
//! renders. **RMS and not peak**, because saturation raises RMS while *lowering*
//! peaks, so peak-matching would compensate in the wrong direction.
//!
//! # Why the dry path goes through a delay line
//! The oversampler is a linear-phase FIR precisely so that its latency is an
//! integer (see [`crate::oversample`]), which means the dry half of the mix knob
//! only has to be *late*. Get this wrong and a 50 % blend combs the top octave --
//! the "the mix knob mysteriously thins the highs" failure. The delay is re-armed
//! whenever the oversampling factor changes.
//!
//! # The DC blocker sits after the shaper, inside the oversampled region
//! Asymmetric shapers "create a DC offset proportional to bias even at silence,
//! and if not subtracting the offset at the output… you'll hear thumps at
//! parameter changes". Putting it at the oversampled rate also means the
//! decimation filter is never handed a DC step.
//!
//! # Latency
//! 4x costs [`OUTER_HALF_LENGTH`](crate::oversample::OUTER_HALF_LENGTH) plus half
//! the inner stage -- 38 samples, 0.79 ms at 48 kHz. 2x costs 32, and **off**
//! costs nothing, which is what makes "off" an honest option rather than merely a
//! cheaper one. The engine has no plugin delay compensation yet, so this is a
//! documented cost of the quality, and it is why the factor is a parameter.

use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, ReadoutDef, RtNode,
    RtNodeFactory, SmoothedParams,
};

use crate::dsp::{
    asymmetric_clip, db_to_linear, hard_clip, soft_clip, usable_sample_rate, DcBlocker, FixedDelay,
    OnePoleTpt,
};
use crate::effect::{index_of, Bypass, CachedDb, BYPASS_PARAM, MIX_PARAM};
use crate::oversample::{OversampleFactor, Oversampler};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.saturator";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "Saturator";

/// One line for a tooltip.
pub const DESCRIPTION: &str =
    "Tape, tube and clip waveshapers with a tilt, level-matched drive and 4x oversampling";

/// Loudest drive the knob offers, in dB.
pub const MAX_DRIVE_DB: f32 = 36.0;

/// Where the tilt pivots, in Hz.
///
/// 700 Hz: low enough that "brighter" pushes real high-mid energy into the
/// shaper, high enough that "darker" does not simply mean "quieter".
pub const TILT_PIVOT_HZ: f32 = 700.0;

/// Drive steps the compensation table is built at.
///
/// One entry per dB plus the endpoint. Linear interpolation between them is well
/// under a tenth of a decibel wrong, and the whole table is 111 floats.
pub const COMPENSATION_STEPS: usize = MAX_DRIVE_DB as usize + 1;

/// Waveshapers the saturator offers, named by mechanism.
///
/// Named by *mechanism* and not by adjective, deliberately: "tape" and "tube"
/// map onto expectations, "warm" and "fat" do not disambiguate anything. Three,
/// because three to five is where the research puts the sweet spot and Saturn's
/// 28 draws documented choice paralysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shaper {
    /// Symmetric soft clip. Odd harmonics only -- the third, fifth, seventh.
    Tape,
    /// Asymmetric soft clip. Even harmonics as well as odd, which is what a
    /// single-ended valve stage does and what "tube" means to a user.
    Tube,
    /// Hard clip with a short quadratic knee. Loudest, harshest, most aliasing
    /// -- which is why the oversampling default matters most here.
    Clip,
}

impl Shaper {
    /// The shaper a switch position selects.
    pub const fn from_choice(choice: usize) -> Self {
        match choice {
            1 => Self::Tube,
            2 => Self::Clip,
            _ => Self::Tape,
        }
    }

    /// Shape one sample.
    #[inline]
    pub fn shape(self, x: f32) -> f32 {
        match self {
            Self::Tape => soft_clip(x),
            Self::Tube => asymmetric_clip(x),
            Self::Clip => hard_clip(x),
        }
    }
}

/// What the saturator exposes.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    ParamDef::choice("type", "Type", 3, 0),
    ParamDef::new(
        "drive",
        "Drive",
        "dB",
        0.0,
        MAX_DRIVE_DB,
        0.0,
        ParamCurve::Db,
    ),
    ParamDef::toggle("auto_gain", "Auto Gain", true),
    ParamDef::new("tone", "Tone", "dB", -12.0, 12.0, 0.0, ParamCurve::Db),
    ParamDef::new(MIX_PARAM, "Mix", "%", 0.0, 100.0, 100.0, ParamCurve::Linear),
    ParamDef::new("output", "Output", "dB", -24.0, 24.0, 0.0, ParamCurve::Db),
    ParamDef::choice("oversample", "Oversampling", 3, 2),
];

/// What the saturator publishes back.
pub static READOUTS: &[ReadoutDef] = &[
    ReadoutDef::new("auto_gain_db", "Applied Auto Gain", "dB"),
    ReadoutDef::new("latency", "Latency", "samples"),
];

/// Index of the applied-compensation readout.
///
/// An auto-gain a user cannot see is an auto-gain they switch off, so the number
/// is published for the panel to print.
pub const READOUT_AUTO_GAIN: usize = 0;
/// Index of the reported latency readout, in samples.
pub const READOUT_LATENCY: usize = 1;

/// The compensation table for every shaper, indexed by whole decibels of drive.
///
/// Built once per node in the factory. Deterministic: the probe signal is a
/// fixed xorshift sequence with a fixed seed, so two builds of the same drive
/// produce the same number and a render is reproducible.
#[derive(Debug)]
struct Compensation {
    table: [[f32; COMPENSATION_STEPS]; 3],
}

impl Compensation {
    /// **Non-RT**: this runs a few hundred thousand samples of probe signal.
    fn measure() -> Self {
        let mut table = [[0.0_f32; COMPENSATION_STEPS]; 3];
        for (index, shaper) in [Shaper::Tape, Shaper::Tube, Shaper::Clip]
            .into_iter()
            .enumerate()
        {
            for (step, slot) in table[index].iter_mut().enumerate() {
                *slot = Self::measure_one(shaper, step as f32);
            }
        }
        Self { table }
    }

    /// The compensation, in dB, for one shaper at one drive setting.
    ///
    /// A fixed deterministic noise sequence at -18 dBFS RMS -- the level the
    /// research names as the sensible probe -- driven, shaped, and measured.
    fn measure_one(shaper: Shaper, drive_db: f32) -> f32 {
        const FRAMES: usize = 8_192;
        const PROBE_RMS: f32 = 0.125; // -18 dBFS
        let drive = db_to_linear(drive_db);
        let mut state: u32 = 0x2545_F491;
        let mut energy_in = 0.0_f64;
        let mut energy_out = 0.0_f64;
        for _ in 0..FRAMES {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let noise = ((state >> 8) as f32 * (2.0 / 16_777_215.0) - 1.0) * PROBE_RMS * 1.732;
            let shaped = shaper.shape(noise * drive);
            energy_in += f64::from(noise).powi(2);
            energy_out += f64::from(shaped).powi(2);
        }
        let rms_in = (energy_in / FRAMES as f64).sqrt();
        let rms_out = (energy_out / FRAMES as f64).sqrt();
        if rms_in <= 0.0 || rms_out <= 0.0 {
            return 0.0;
        }
        -(20.0 * (rms_out / rms_in).log10()) as f32
    }

    /// The compensation for `shaper` at `drive_db`, interpolated.
    #[inline]
    fn at(&self, shaper: Shaper, drive_db: f32) -> f32 {
        let row = &self.table[match shaper {
            Shaper::Tape => 0,
            Shaper::Tube => 1,
            Shaper::Clip => 2,
        }];
        let position = drive_db.clamp(0.0, MAX_DRIVE_DB);
        let low = position.floor() as usize;
        let high = (low + 1).min(COMPENSATION_STEPS - 1);
        let t = position - low as f32;
        row[low] * (1.0 - t) + row[high] * t
    }
}

/// Parameter indices, resolved once off the audio thread.
#[derive(Debug, Clone, Copy)]
struct Indices {
    bypass: usize,
    shaper: usize,
    drive: usize,
    auto_gain: usize,
    tone: usize,
    mix: usize,
    output: usize,
    oversample: usize,
}

impl Indices {
    fn new(block: &ParamBlock) -> Self {
        Self {
            bypass: index_of(block, BYPASS_PARAM),
            shaper: index_of(block, "type"),
            drive: index_of(block, "drive"),
            auto_gain: index_of(block, "auto_gain"),
            tone: index_of(block, "tone"),
            mix: index_of(block, MIX_PARAM),
            output: index_of(block, "output"),
            oversample: index_of(block, "oversample"),
        }
    }
}

/// One channel's shaping chain.
struct Channel {
    tilt: OnePoleTpt,
    over: Oversampler,
    dc: DcBlocker,
    dry: FixedDelay,
}

impl Channel {
    fn new(rate: f32, factor: OversampleFactor, max_latency: usize) -> Self {
        let mut tilt = OnePoleTpt::new();
        tilt.set_cutoff(TILT_PIVOT_HZ, rate);
        Self {
            tilt,
            over: Oversampler::new(factor),
            // At the oversampled rate, but a 5 Hz corner at 4x is still 5 Hz --
            // the coefficient is derived from the rate, which is the point.
            dc: DcBlocker::new(DcBlocker::DEFAULT_HZ, rate * 4.0),
            dry: FixedDelay::new(max_latency),
        }
    }

    fn reset(&mut self) {
        self.tilt.reset();
        self.over.reset();
        self.dc.reset();
        self.dry.clear();
    }
}

/// The saturator node.
pub struct SaturatorNode {
    params: SmoothedParams,
    index: Indices,
    bypass: Bypass,
    drive: CachedDb,
    output: CachedDb,
    channels: [Channel; 2],
    compensation: Compensation,
    /// Last applied compensation, for the readout.
    applied_db: f32,
}

impl SaturatorNode {
    /// **Non-RT**: allocates, designs the halfbands and measures the
    /// compensation table.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        let index = Indices::new(&params);
        let smoothed = SmoothedParams::new(params, sample_rate);
        let rate = usable_sample_rate(sample_rate);
        let factor = OversampleFactor::from_choice(smoothed.choice(index.oversample));
        // Enough for the deepest factor, so switching never allocates.
        let max_latency = Oversampler::new(OversampleFactor::Four).latency() + 1;
        let mut node = Self {
            drive: CachedDb::new(index.drive, &smoothed),
            output: CachedDb::new(index.output, &smoothed),
            params: smoothed,
            index,
            bypass: Bypass::new(max_block, sample_rate),
            channels: [
                Channel::new(rate, factor, max_latency),
                Channel::new(rate, factor, max_latency),
            ],
            compensation: Compensation::measure(),
            applied_db: 0.0,
        };
        node.arm_latency();
        node
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }
    }

    /// Match the dry delay to the oversampler's latency and publish it.
    fn arm_latency(&mut self) {
        let latency = self.channels[0].over.latency();
        for channel in &mut self.channels {
            channel.dry.set_length(latency);
        }
        self.params
            .block()
            .set_readout(READOUT_LATENCY, latency as f32);
    }

    /// Apply the oversampling switch, if it moved.
    fn refresh_factor(&mut self) {
        let factor = OversampleFactor::from_choice(self.params.choice(self.index.oversample));
        if factor == self.channels[0].over.factor() {
            return;
        }
        for channel in &mut self.channels {
            channel.over.set_factor(factor);
        }
        self.arm_latency();
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        self.refresh_factor();
        let shaper = Shaper::from_choice(self.params.choice(self.index.shaper));
        let auto_gain = self.params.choice(self.index.auto_gain) != 0;
        let mut applied = 0.0_f32;

        // Split the borrows so the shaper closure can hold the DC blocker.
        let Self {
            params,
            index,
            drive: drive_param,
            output: output_param,
            channels,
            compensation,
            ..
        } = self;
        let [chan_l, chan_r] = channels;

        let buffers = ctx.buffers.as_mut_slices();
        let [left, right] = buffers;
        for frame in 0..frames {
            let drive = drive_param.next(params);
            let tone = params.next(index.tone);
            let mix = params.next(index.mix) * 0.01;
            let output = output_param.next(params);
            params.next(index.drive);

            // The tilt: a first-order split at the pivot, the two halves scaled
            // in opposite directions. Positive tone drives the highs harder,
            // which is what "Keep Low" does on the plugin everybody uses.
            let low_gain = db_to_linear(-tone * 0.5);
            let high_gain = db_to_linear(tone * 0.5);
            let compensate = if auto_gain {
                compensation.at(shaper, params.value(index.drive))
            } else {
                0.0
            };
            applied = compensate;
            let make_up = db_to_linear(compensate) * output;

            for (channel, buffer) in [(&mut *chan_l, &mut *left), (&mut *chan_r, &mut *right)] {
                let dry = buffer[frame];
                let low = channel.tilt.low(dry);
                let tilted = low * low_gain + (dry - low) * high_gain;

                let dc = &mut channel.dc;
                let wet = channel
                    .over
                    .process(tilted * drive, |s| dc.process(shaper.shape(s)));

                // The dry half is delayed by exactly the oversampler's latency,
                // so the blend is phase-correct at every mix setting.
                let aligned = channel.dry.process(dry);
                buffer[frame] = aligned * (1.0 - mix) + wet * make_up * mix;
            }
        }

        self.applied_db = applied;
        self.params.block().set_readout(READOUT_AUTO_GAIN, applied);
    }
}

impl RtNode for SaturatorNode {
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

/// Builds [`SaturatorNode`]s.
#[derive(Debug, Default)]
pub struct SaturatorFactory;

impl SaturatorFactory {
    pub const fn new() -> Self {
        Self
    }
}

impl RtNodeFactory for SaturatorFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for SaturatorFactory {
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
        Box::new(SaturatorNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        amplitude_at, db, level_db_at, render_mono, render_stereo, rms_mono, sine, Harness,
    };

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&SaturatorFactory::new(), SR, 1_024)
    }

    #[test]
    fn a_fresh_saturator_passes_the_signal_through_at_the_same_level() {
        // Drive 0 dB with a `tanh`-family shaper is not an *identity* -- the
        // curve still bends near full scale -- but it must not change the level
        // or the tone of ordinary material.
        let mut h = harness();
        let input = sine(1_000.0, 24_000, SR, 0.2);
        let out = render_mono(&mut h, &input);
        let change = db(rms_mono(&out[8_000..]) / rms_mono(&input[8_000..]));
        assert!(
            change.abs() < 0.2,
            "a fresh saturator moved the level {change} dB"
        );
    }

    #[test]
    fn the_tape_shaper_makes_odd_harmonics_and_the_tube_shaper_makes_even_ones() {
        // The claim each name is making, measured. A symmetric curve *cannot*
        // produce a second harmonic; an asymmetric one must.
        let harmonics = |shaper: f32| {
            let mut h = harness();
            h.set("type", shaper);
            h.set("drive", 24.0);
            h.set("auto_gain", 0.0);
            let input = sine(1_000.0, 48_000, SR, 0.5);
            let out = render_mono(&mut h, &input);
            let window = &out[16_000..];
            (
                level_db_at(window, 1_000.0, SR),
                level_db_at(window, 2_000.0, SR),
                level_db_at(window, 3_000.0, SR),
            )
        };

        let (tape_f1, tape_f2, tape_f3) = harmonics(0.0);
        let (_, tube_f2, tube_f3) = harmonics(1.0);
        println!(
            "tape: f1 {tape_f1:.1} f2 {tape_f2:.1} f3 {tape_f3:.1} dB |              tube: f2 {tube_f2:.1} f3 {tube_f3:.1} dB"
        );

        // Tape is odd-symmetric, so it has a strong third and essentially no
        // second -- a symmetric curve *cannot* produce even harmonics, and what
        // little shows up is the arithmetic's rounding.
        assert!(
            tape_f3 > tape_f1 - 40.0,
            "no third harmonic at all: {tape_f3}"
        );
        assert!(
            tape_f2 < tape_f3 - 25.0,
            "a symmetric shaper produced a second harmonic of {tape_f2} dB              against a third of {tape_f3} dB"
        );

        // Tube is not odd, so it must. The comparison that matters is against
        // the symmetric shaper at the same drive, not against its own third:
        // deep saturation of any kind is third-harmonic dominated.
        assert!(
            tube_f2 > tape_f2 + 20.0,
            "the asymmetric shaper's second harmonic ({tube_f2:.1} dB) is no bigger              than the symmetric one's ({tape_f2:.1} dB)"
        );
        assert!(tube_f3 > tape_f3 - 12.0, "the tube lost its third harmonic");
    }

    #[test]
    fn oversampling_suppresses_aliasing_against_its_own_off_setting() {
        // The measurement the feature exists for. 7 kHz into a hard clipper: the
        // seventh harmonic is 49 kHz, which folds to 1 kHz at a 48 kHz rate --
        // a frequency nothing harmonic can occupy.
        let alias_db = |factor: f32| {
            let mut h = harness();
            h.set("type", 2.0); // clip
            h.set("drive", 24.0);
            h.set("auto_gain", 0.0);
            h.set("oversample", factor);
            let input = sine(7_000.0, 48_000, SR, 0.7);
            let out = render_mono(&mut h, &input);
            level_db_at(&out[16_000..], 1_000.0, SR)
        };

        let none = alias_db(0.0);
        let four = alias_db(2.0);
        let suppression = none - four;
        println!("aliasing at 1 kHz: off {none:.1} dB, 4x {four:.1} dB ({suppression:.1} dB down)");
        assert!(
            suppression > 20.0,
            "4x only bought {suppression:.1} dB of alias suppression"
        );
    }

    #[test]
    fn drive_compensation_keeps_the_level_where_it_was() {
        // The property that makes an A/B honest, across the whole knob.
        let mut worst = 0.0_f32;
        for drive in [0.0_f32, 6.0, 12.0, 24.0, 36.0] {
            for shaper in [0.0_f32, 1.0, 2.0] {
                let mut h = harness();
                h.set("type", shaper);
                h.set("drive", drive);
                let input = sine(220.0, 48_000, SR, 0.2);
                let out = render_mono(&mut h, &input);
                let change = db(rms_mono(&out[16_000..]) / rms_mono(&input[16_000..]));
                worst = worst.max(change.abs());
                assert!(
                    change.abs() < 1.5,
                    "type {shaper} at {drive} dB drive moved the level {change:.2} dB"
                );
            }
        }
        println!("worst level deviation across the drive range: {worst:.2} dB");
    }

    #[test]
    fn switching_auto_gain_off_lets_the_level_run_away_again() {
        // The control has to do something, or the previous test proves nothing.
        let mut h = harness();
        h.set("drive", 24.0);
        h.set("auto_gain", 0.0);
        let input = sine(220.0, 48_000, SR, 0.2);
        let out = render_mono(&mut h, &input);
        let change = db(rms_mono(&out[16_000..]) / rms_mono(&input[16_000..]));
        assert!(change > 6.0, "uncompensated drive only added {change} dB");
    }

    #[test]
    fn the_compensation_is_readable_so_a_panel_can_print_it() {
        let mut h = harness();
        h.set("drive", 24.0);
        render_mono(&mut h, &sine(220.0, 4_096, SR, 0.3));
        let applied = h.readout("auto_gain_db");
        assert!(applied < -3.0, "24 dB of drive should pull back: {applied}");

        h.set("auto_gain", 0.0);
        render_mono(&mut h, &sine(220.0, 4_096, SR, 0.3));
        assert_eq!(h.readout("auto_gain_db"), 0.0);
    }

    #[test]
    fn the_compensation_table_is_deterministic() {
        // Two builds must agree, or a render is not reproducible.
        let a = Compensation::measure();
        let b = Compensation::measure();
        assert_eq!(a.table, b.table);
        // And it is monotone: more drive can only ever need more pulling back.
        for row in &a.table {
            for pair in row.windows(2) {
                assert!(pair[1] <= pair[0] + 1e-4, "{pair:?}");
            }
        }
    }

    #[test]
    fn the_tone_control_is_a_tilt_and_not_a_volume_knob() {
        let response = |tone: f32, hz: f64| {
            let mut h = harness();
            h.set("tone", tone);
            h.set("drive", 0.0);
            h.set("auto_gain", 0.0);
            let input = sine(hz, 48_000, SR, 0.1);
            let out = render_mono(&mut h, &input);
            level_db_at(&out[16_000..], hz, SR) - level_db_at(&input[16_000..], hz, SR)
        };

        // Positive tone: highs up, lows down, by about half the setting each.
        let low = response(12.0, 100.0);
        let high = response(12.0, 8_000.0);
        assert!(low < -4.0, "the bottom should come down, got {low}");
        assert!(high > 4.0, "the top should go up, got {high}");
        // And it is symmetric about the pivot.
        assert!((low + high).abs() < 1.5, "{low} and {high} do not balance");
    }

    #[test]
    fn the_latency_is_reported_and_the_dry_path_is_aligned_to_it() {
        let mut h = harness();
        render_mono(&mut h, &sine(1_000.0, 4_096, SR, 0.1));
        let four = h.readout("latency");
        assert!(four > 0.0);

        h.set("oversample", 0.0);
        render_mono(&mut h, &sine(1_000.0, 4_096, SR, 0.1));
        assert_eq!(h.readout("latency"), 0.0, "off must be free");

        // The alignment claim: at 50 % mix with oversampling on, a full-band
        // signal must not comb. Compare the top octave against the dry level.
        let mut h = harness();
        h.set(MIX_PARAM, 50.0);
        h.set("drive", 0.0);
        h.set("auto_gain", 0.0);
        let input = sine(12_000.0, 48_000, SR, 0.15);
        let out = render_mono(&mut h, &input);
        let loss =
            level_db_at(&out[16_000..], 12_000.0, SR) - level_db_at(&input[16_000..], 12_000.0, SR);
        assert!(
            loss.abs() < 1.0,
            "a 50 % blend lost {loss:.2} dB at 12 kHz; the dry path is misaligned"
        );
    }

    #[test]
    fn the_mix_knob_reaches_the_dry_signal() {
        let mut h = harness();
        h.set("drive", 30.0);
        h.set(MIX_PARAM, 0.0);
        h.set("oversample", 0.0);
        let input: Vec<[f32; 2]> = (0..8_192)
            .map(|n| [(n as f32 * 0.07).sin() * 0.4, (n as f32 * 0.03).sin() * 0.4])
            .collect();
        let out = render_stereo(&mut h, &input);
        // Oversampling off means the dry delay is zero, so 0 % mix is exact.
        assert_eq!(&out[4_096..], &input[4_096..]);
    }

    #[test]
    fn a_bypassed_saturator_is_the_input_again() {
        let mut h = harness();
        h.set("drive", 30.0);
        let input: Vec<[f32; 2]> = (0..8_192)
            .map(|n| [(n as f32 * 0.05).sin() * 0.5, (n as f32 * 0.02).sin() * 0.5])
            .collect();
        render_stereo(&mut h, &input);

        h.set("bypass", 1.0);
        render_stereo(&mut h, &input);
        let out = render_stereo(&mut h, &input);
        assert_eq!(out, input);
    }

    #[test]
    fn the_asymmetric_shaper_does_not_write_a_dc_offset_into_the_mix() {
        // The named failure of every tube-style shaper: an offset "proportional
        // to bias even at silence", and thumps at parameter changes.
        let mut h = harness();
        h.set("type", 1.0);
        h.set("drive", 30.0);
        let input = sine(220.0, 48_000, SR, 0.6);
        let out = render_mono(&mut h, &input);
        let mean: f64 =
            out[24_000..].iter().map(|s| f64::from(*s)).sum::<f64>() / (out.len() - 24_000) as f64;
        assert!(mean.abs() < 1e-3, "the output carries {mean} of DC");
        // And the fundamental survived, so the blocker is not eating the audio.
        assert!(amplitude_at(&out[24_000..], 220.0, SR) > 0.1);
    }
}
