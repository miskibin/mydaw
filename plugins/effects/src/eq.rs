//! `effects.eq` -- a five-band parametric equaliser.
//!
//! ```text
//!   in -> [HP 12/24] -> [low shelf] -> [bell 1] -> [bell 2] -> [high shelf] -> [output] -> out
//! ```
//!
//! # Why five fixed bands and not "add a band"
//! A fixed chain is a fixed allocation, which is what an audio thread wants, and
//! this particular five is the chain a mixer strip actually needs: something to
//! take the rumble off, something for the bottom, two things to fix problems
//! with, and something for the top. An EQ with an unbounded band count would
//! have to allocate on an edit or over-allocate for a case nobody hits.
//!
//! Every band has its own `enabled` switch, so "add a band" is "turn one on",
//! and a band that is off costs one branch per block rather than a chain rebuild.
//! Band indices are therefore **stable by construction** -- the complaint that
//! deleting a band renumbers the others and breaks host automation cannot arise
//! here, because nothing is ever deleted.
//!
//! # Coefficients are recomputed per sample, but only while a knob moves
//! Recomputing a biquad once per block and holding it is the usual shortcut, and
//! it is exactly the zipper users complain about: an 18 dB gain change becomes a
//! staircase whose steps are the block size. Recomputing every sample all the
//! time would be two transcendentals per band per sample for ever.
//!
//! So a band asks its parameters whether any of them is mid-ramp. If none is --
//! which is every block in which nobody is touching the EQ -- the cached
//! coefficients are used and the inner loop is five biquads and nothing else. If
//! one is, that band recomputes per sample for the 10 ms the ramp lasts. The
//! bill is two `sin`/`cos` per sample for one band for a hundredth of a second
//! per gesture.
//!
//! # 24 dB/octave is two *staggered* sections
//! Cascading two 12 dB/octave highpasses at the same Q puts the corner 6 dB down
//! instead of 3 and ripples in the passband. A fourth-order Butterworth needs its
//! two sections at `1/(2 cos 22.5°)` and `1/(2 cos 67.5°)` --
//! [`BUTTERWORTH_Q4`](crate::biquad::BUTTERWORTH_Q4) -- which is what this uses
//! and what `biquad.rs` asserts.
//!
//! # The curve the UI will draw is the curve the audio takes
//! [`magnitude_db_at`] evaluates the same coefficient set the audio path is
//! running, from the closed form in [`BiquadCoeffs::magnitude_at`]. A second
//! implementation for the display would be a second implementation to keep
//! right; this way the response test and the future curve renderer are reading
//! the same numbers.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use mydaw_rt_core::{
    ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ProcessCtx, RtNode, RtNodeFactory,
    SmoothedParams, CHANNELS,
};

use crate::biquad::{
    bandpass, high_shelf, highpass, low_shelf, lowpass, peaking, Biquad, BiquadCoeffs,
    BUTTERWORTH_Q4,
};
use crate::effect::{index_of, Bypass, CachedDb, BYPASS_PARAM};

/// Factory `type_id`, and the id a project file records.
pub const NODE_TYPE_ID: &str = "effects.eq";

/// Name for a menu.
pub const DISPLAY_NAME: &str = "EQ";

/// One line for a tooltip.
pub const DESCRIPTION: &str =
    "Five bands: highpass, low shelf, two parametric bells and a high shelf";

/// Widest boost or cut a band offers, in dB.
pub const MAX_BAND_GAIN_DB: f32 = 18.0;

/// Lowest frequency any band can sit at.
pub const MIN_FREQ_HZ: f32 = 20.0;

/// Highest frequency any band can sit at.
pub const MAX_FREQ_HZ: f32 = 20_000.0;

/// Bands in the chain.
pub const BANDS: usize = 5;

/// Which of the five a band is, and therefore what shape it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandKind {
    /// 12 or 24 dB/octave highpass. No gain.
    Highpass,
    LowShelf,
    Bell,
    HighShelf,
}

/// The parameter-id prefix of each band, in signal order.
pub const BAND_IDS: [&str; BANDS] = ["hp", "low", "bell1", "bell2", "high"];

/// What each band is.
pub const BAND_KINDS: [BandKind; BANDS] = [
    BandKind::Highpass,
    BandKind::LowShelf,
    BandKind::Bell,
    BandKind::Bell,
    BandKind::HighShelf,
];

/// What the EQ exposes.
///
/// The order is signal order, so a UI that lays the list out top to bottom gets
/// the chain for free.
pub static PARAMS: &[ParamDef] = &[
    ParamDef::toggle(BYPASS_PARAM, "Bypass", false),
    // --- highpass -------------------------------------------------------
    // Off by default: a freshly inserted EQ must be bit-transparent, and a
    // highpass at 80 Hz is not.
    ParamDef::toggle("hp.enabled", "HP On", false),
    ParamDef::new(
        "hp.freq",
        "HP Freq",
        "Hz",
        MIN_FREQ_HZ,
        1_000.0,
        80.0,
        ParamCurve::Log,
    ),
    // Two positions: 12 and 24 dB/octave, the two everybody expects.
    ParamDef::choice("hp.slope", "HP Slope", 2, 1),
    // --- low shelf ------------------------------------------------------
    ParamDef::toggle("low.enabled", "Low On", true),
    ParamDef::new(
        "low.freq",
        "Low Freq",
        "Hz",
        MIN_FREQ_HZ,
        MAX_FREQ_HZ,
        120.0,
        ParamCurve::Log,
    ),
    ParamDef::new(
        "low.gain",
        "Low Gain",
        "dB",
        -MAX_BAND_GAIN_DB,
        MAX_BAND_GAIN_DB,
        0.0,
        ParamCurve::Db,
    ),
    ParamDef::new("low.q", "Low Q", "", 0.1, 10.0, 0.7, ParamCurve::Log),
    // --- bell 1 ---------------------------------------------------------
    ParamDef::toggle("bell1.enabled", "Bell 1 On", true),
    ParamDef::new(
        "bell1.freq",
        "Bell 1 Freq",
        "Hz",
        MIN_FREQ_HZ,
        MAX_FREQ_HZ,
        500.0,
        ParamCurve::Log,
    ),
    ParamDef::new(
        "bell1.gain",
        "Bell 1 Gain",
        "dB",
        -MAX_BAND_GAIN_DB,
        MAX_BAND_GAIN_DB,
        0.0,
        ParamCurve::Db,
    ),
    // Q 1.0 and not 0.707: Pro-Q's default is the value every user's hands are
    // trained on, and 0.707 reads as too wide to be surgical.
    ParamDef::new("bell1.q", "Bell 1 Q", "", 0.1, 10.0, 1.0, ParamCurve::Log),
    // --- bell 2 ---------------------------------------------------------
    ParamDef::toggle("bell2.enabled", "Bell 2 On", true),
    ParamDef::new(
        "bell2.freq",
        "Bell 2 Freq",
        "Hz",
        MIN_FREQ_HZ,
        MAX_FREQ_HZ,
        3_000.0,
        ParamCurve::Log,
    ),
    ParamDef::new(
        "bell2.gain",
        "Bell 2 Gain",
        "dB",
        -MAX_BAND_GAIN_DB,
        MAX_BAND_GAIN_DB,
        0.0,
        ParamCurve::Db,
    ),
    ParamDef::new("bell2.q", "Bell 2 Q", "", 0.1, 10.0, 1.0, ParamCurve::Log),
    // --- high shelf -----------------------------------------------------
    ParamDef::toggle("high.enabled", "High On", true),
    ParamDef::new(
        "high.freq",
        "High Freq",
        "Hz",
        MIN_FREQ_HZ,
        MAX_FREQ_HZ,
        8_000.0,
        ParamCurve::Log,
    ),
    ParamDef::new(
        "high.gain",
        "High Gain",
        "dB",
        -MAX_BAND_GAIN_DB,
        MAX_BAND_GAIN_DB,
        0.0,
        ParamCurve::Db,
    ),
    ParamDef::new("high.q", "High Q", "", 0.1, 10.0, 0.7, ParamCurve::Log),
    // --- output ---------------------------------------------------------
    ParamDef::new(
        "output",
        "Output",
        "dB",
        -MAX_BAND_GAIN_DB,
        MAX_BAND_GAIN_DB,
        0.0,
        ParamCurve::Db,
    ),
];

/// Where one band's parameters live in the block.
#[derive(Debug, Clone, Copy)]
struct BandIndices {
    kind: BandKind,
    enabled: usize,
    freq: usize,
    /// `usize::MAX` for the highpass, which has no gain.
    gain: usize,
    /// The Q knob, or the slope switch for the highpass.
    q: usize,
    slope: usize,
}

impl BandIndices {
    fn new(block: &ParamBlock, band: usize) -> Self {
        let id = BAND_IDS[band];
        let kind = BAND_KINDS[band];
        let named = |suffix: &str| -> usize {
            block
                .index_of(&format!("{id}.{suffix}"))
                .unwrap_or_else(|| panic!("band '{id}' declares no '{suffix}'"))
        };
        match kind {
            BandKind::Highpass => Self {
                kind,
                enabled: named("enabled"),
                freq: named("freq"),
                gain: usize::MAX,
                q: usize::MAX,
                slope: named("slope"),
            },
            _ => Self {
                kind,
                enabled: named("enabled"),
                freq: named("freq"),
                gain: named("gain"),
                q: named("q"),
                slope: usize::MAX,
            },
        }
    }

    /// Whether any of this band's continuous parameters is mid-ramp.
    fn is_moving(&self, params: &SmoothedParams) -> bool {
        params.is_smoothing(self.freq)
            || (self.gain != usize::MAX && params.is_smoothing(self.gain))
            || (self.q != usize::MAX && params.is_smoothing(self.q))
    }
}

/// The coefficients one band is running, and its filter memory.
#[derive(Debug)]
struct Band {
    /// Up to two sections; only the highpass at 24 dB/octave uses both.
    coeffs: [BiquadCoeffs; 2],
    state: [[Biquad; 2]; CHANNELS],
    sections: usize,
}

impl Band {
    fn new() -> Self {
        Self {
            coeffs: [BiquadCoeffs::BYPASS; 2],
            state: [[Biquad::new(); 2]; CHANNELS],
            sections: 1,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.state {
            for section in channel {
                section.reset();
            }
        }
    }

    #[inline]
    fn process(&mut self, channel: usize, x: f64) -> f64 {
        let mut y = x;
        for section in 0..self.sections {
            y = self.state[channel][section].process(y, &self.coeffs[section]);
        }
        y
    }
}

/// Compute one band's coefficients from raw parameter values.
///
/// Free-standing, and taking values rather than a node, so
/// [`magnitude_db_at`] can call it with what a [`ParamBlock`] holds and get the
/// same answer the audio thread would.
fn band_coefficients(
    kind: BandKind,
    freq: f64,
    gain_db: f64,
    q: f64,
    slope_choice: usize,
    sample_rate: f64,
) -> ([BiquadCoeffs; 2], usize) {
    match kind {
        BandKind::Highpass => {
            if slope_choice == 0 {
                (
                    [
                        highpass(freq, std::f64::consts::FRAC_1_SQRT_2, sample_rate),
                        BiquadCoeffs::BYPASS,
                    ],
                    1,
                )
            } else {
                (
                    [
                        highpass(freq, BUTTERWORTH_Q4[0], sample_rate),
                        highpass(freq, BUTTERWORTH_Q4[1], sample_rate),
                    ],
                    2,
                )
            }
        }
        BandKind::LowShelf => (
            [
                low_shelf(freq, q, gain_db, sample_rate),
                BiquadCoeffs::BYPASS,
            ],
            1,
        ),
        BandKind::Bell => (
            [peaking(freq, q, gain_db, sample_rate), BiquadCoeffs::BYPASS],
            1,
        ),
        BandKind::HighShelf => (
            [
                high_shelf(freq, q, gain_db, sample_rate),
                BiquadCoeffs::BYPASS,
            ],
            1,
        ),
    }
}

/// Coefficients for hold-to-solo: a band of the *source*, not the EQ's effect.
fn listen_coefficients(
    kind: BandKind,
    freq: f64,
    q: f64,
    slope_choice: usize,
    sample_rate: f64,
) -> ([BiquadCoeffs; 2], usize) {
    match kind {
        BandKind::Highpass => {
            if slope_choice == 0 {
                (
                    [
                        lowpass(freq, std::f64::consts::FRAC_1_SQRT_2, sample_rate),
                        BiquadCoeffs::BYPASS,
                    ],
                    1,
                )
            } else {
                (
                    [
                        lowpass(freq, BUTTERWORTH_Q4[0], sample_rate),
                        lowpass(freq, BUTTERWORTH_Q4[1], sample_rate),
                    ],
                    2,
                )
            }
        }
        BandKind::LowShelf => (
            [
                lowpass(freq, std::f64::consts::FRAC_1_SQRT_2, sample_rate),
                BiquadCoeffs::BYPASS,
            ],
            1,
        ),
        BandKind::Bell => ([bandpass(freq, q, sample_rate), BiquadCoeffs::BYPASS], 1),
        BandKind::HighShelf => (
            [
                highpass(freq, std::f64::consts::FRAC_1_SQRT_2, sample_rate),
                BiquadCoeffs::BYPASS,
            ],
            1,
        ),
    }
}

/// The EQ's magnitude response at `hz`, in dB, for the settings in `block`.
///
/// **Non-RT.** This is what the future curve display samples, and what
/// `tests/eq_dsp.rs` checks the audio path against. It reads the *stored*
/// parameter values, so it describes where the EQ is heading rather than where a
/// ramp has got to -- which is what a curve should draw.
///
/// Returns `0.0` for a block that is not an EQ's.
pub fn magnitude_db_at(block: &ParamBlock, hz: f64, sample_rate: f64) -> f64 {
    let mut magnitude = 1.0_f64;
    for band in 0..BANDS {
        let id = BAND_IDS[band];
        let kind = BAND_KINDS[band];
        let get = |suffix: &str| block.get_by_id(&format!("{id}.{suffix}"));
        if get("enabled").unwrap_or(0.0) < 0.5 {
            continue;
        }
        let Some(freq) = get("freq") else { continue };
        let gain = get("gain").unwrap_or(0.0);
        let q = get("q").unwrap_or(0.707);
        let slope = get("slope").unwrap_or(0.0).round().max(0.0) as usize;
        let (coeffs, sections) = band_coefficients(
            kind,
            f64::from(freq),
            f64::from(gain),
            f64::from(q),
            slope,
            sample_rate,
        );
        for section in coeffs.iter().take(sections) {
            magnitude *= section.magnitude_at(hz, sample_rate);
        }
    }
    let output = block.get_by_id("output").unwrap_or(0.0);
    20.0 * magnitude.max(1.0e-12).log10() + f64::from(output)
}

/// Sample [`magnitude_db_at`] at `points` frequencies, logarithmically spaced
/// from [`MIN_FREQ_HZ`] to [`MAX_FREQ_HZ`].
///
/// **Non-RT**: allocates. The shape a curve display wants, computed once here so
/// the shell does not grow its own copy of the band maths.
pub fn magnitude_curve(block: &ParamBlock, sample_rate: f64, points: usize) -> Vec<(f64, f64)> {
    let points = points.max(2);
    let (low, high) = (f64::from(MIN_FREQ_HZ), f64::from(MAX_FREQ_HZ));
    (0..points)
        .map(|i| {
            let t = i as f64 / (points - 1) as f64;
            let hz = low * (high / low).powf(t);
            (hz, magnitude_db_at(block, hz, sample_rate))
        })
        .collect()
}

/// Hold-to-solo is off.
pub const LISTEN_OFF: i32 = -1;

/// The EQ node.
pub struct EqNode {
    params: SmoothedParams,
    bypass_index: usize,
    output: CachedDb,
    bands: [BandIndices; BANDS],
    filters: [Band; BANDS],
    listen_filter: Band,
    listen: Arc<AtomicI32>,
    last_listen: i32,
    bypass: Bypass,
    sample_rate: f64,
}

impl EqNode {
    /// **Non-RT**: allocates.
    pub fn new(params: Arc<ParamBlock>, sample_rate: f64, max_block: usize) -> Self {
        Self::new_with_listen(
            params,
            sample_rate,
            max_block,
            Arc::new(AtomicI32::new(LISTEN_OFF)),
        )
    }

    /// Same as [`new`](Self::new), sharing a listen word the shell can store.
    pub fn new_with_listen(
        params: Arc<ParamBlock>,
        sample_rate: f64,
        max_block: usize,
        listen: Arc<AtomicI32>,
    ) -> Self {
        let bands = std::array::from_fn(|band| BandIndices::new(&params, band));
        let bypass_index = index_of(&params, BYPASS_PARAM);
        let smoothed = SmoothedParams::new(params, sample_rate);
        let mut node = Self {
            output: CachedDb::new(index_of_output(&smoothed), &smoothed),
            params: smoothed,
            bypass_index,
            bands,
            filters: std::array::from_fn(|_| Band::new()),
            listen_filter: Band::new(),
            listen,
            last_listen: LISTEN_OFF,
            bypass: Bypass::new(max_block, sample_rate),
            sample_rate: if sample_rate.is_finite() && sample_rate > 1.0 {
                sample_rate
            } else {
                crate::dsp::FALLBACK_SAMPLE_RATE
            },
        };
        // Start at the settings, not sweeping towards them: a rebuilt graph
        // picks the EQ up exactly where the old one left it.
        node.refresh_all();
        node
    }

    fn reset(&mut self) {
        for band in &mut self.filters {
            band.reset();
        }
    }

    /// Recompute every band's coefficients from the current smoothed values.
    fn refresh_all(&mut self) {
        for band in 0..BANDS {
            self.refresh(band);
        }
    }

    /// Recompute one band's coefficients from the current smoothed values.
    #[inline]
    fn refresh(&mut self, band: usize) {
        let index = self.bands[band];
        let freq = f64::from(self.params.value(index.freq));
        let gain = if index.gain == usize::MAX {
            0.0
        } else {
            f64::from(self.params.value(index.gain))
        };
        let q = if index.q == usize::MAX {
            std::f64::consts::FRAC_1_SQRT_2
        } else {
            f64::from(self.params.value(index.q))
        };
        let slope = if index.slope == usize::MAX {
            0
        } else {
            self.params.choice(index.slope)
        };
        let (coeffs, sections) =
            band_coefficients(index.kind, freq, gain, q, slope, self.sample_rate);
        self.filters[band].coeffs = coeffs;
        self.filters[band].sections = sections;
    }

    fn render(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames;
        // Decided once per block: which bands are in the chain at all, and which
        // of those have a knob moving and therefore need per-sample coefficients.
        let mut active = [false; BANDS];
        let mut moving = [false; BANDS];
        for (band, index) in self.bands.iter().enumerate() {
            active[band] = self.params.choice(index.enabled) != 0;
            moving[band] = active[band] && index.is_moving(&self.params);
        }

        let channels = ctx.buffers.as_mut_slices();
        let [left, right] = channels;
        for frame in 0..frames {
            // Advance every band's parameters whether or not the band is on, so
            // a band switched on mid-ramp does not inherit a stale position.
            for (band, &moves) in moving.iter().enumerate() {
                let index = self.bands[band];
                self.params.next(index.freq);
                if index.gain != usize::MAX {
                    self.params.next(index.gain);
                }
                if index.q != usize::MAX {
                    self.params.next(index.q);
                }
                if moves {
                    self.refresh(band);
                }
            }
            let output = self.output.next(&mut self.params);

            let mut l = f64::from(left[frame]);
            let mut r = f64::from(right[frame]);
            for (filter, on) in self.filters.iter_mut().zip(active) {
                if on {
                    l = filter.process(0, l);
                    r = filter.process(1, r);
                }
            }
            left[frame] = l as f32 * output;
            right[frame] = r as f32 * output;
        }
    }

    /// Band-pass (or HP-as-LP / shelf-region) of the *source*, not the EQ.
    fn render_listen(&mut self, ctx: &mut ProcessCtx, band: usize) {
        let index = self.bands[band];
        let freq = f64::from(self.params.value(index.freq))
            .clamp(f64::from(MIN_FREQ_HZ), f64::from(MAX_FREQ_HZ));
        let q = if index.q == usize::MAX {
            std::f64::consts::FRAC_1_SQRT_2
        } else {
            f64::from(self.params.value(index.q)).clamp(0.1, 10.0)
        };
        let slope = if index.slope == usize::MAX {
            0
        } else {
            self.params.choice(index.slope)
        };
        let (coeffs, sections) = listen_coefficients(index.kind, freq, q, slope, self.sample_rate);
        self.listen_filter.coeffs = coeffs;
        self.listen_filter.sections = sections;

        let frames = ctx.frames;
        let channels = ctx.buffers.as_mut_slices();
        let [left, right] = channels;
        for frame in 0..frames {
            left[frame] = self.listen_filter.process(0, f64::from(left[frame])) as f32;
            right[frame] = self.listen_filter.process(1, f64::from(right[frame])) as f32;
        }
    }
}

/// The output parameter's index. Split out so `new` can build the cache before
/// the struct exists.
fn index_of_output(params: &SmoothedParams) -> usize {
    params
        .block()
        .index_of("output")
        .expect("the EQ declares an output parameter")
}

impl RtNode for EqNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        self.params.begin_block();
        let listen = self.listen.load(Ordering::Relaxed);
        if listen >= 0 && (listen as usize) < BANDS {
            if listen != self.last_listen {
                self.listen_filter.reset();
                self.last_listen = listen;
            }
            let _ = self.bypass.begin(ctx, false);
            self.render_listen(ctx, listen as usize);
            self.params.skip_all(ctx.frames);
            return;
        }
        self.last_listen = LISTEN_OFF;

        let bypassed = self.params.choice(self.bypass_index) != 0;
        let state = self.bypass.begin(ctx, bypassed);

        if state.processes() {
            if state.resets() {
                self.reset();
            }
            self.render(ctx);
            // A band whose switch moved needs its coefficients before the *next*
            // block, and a band that stopped moving needs them exact rather than
            // one ramp step short.
            for band in 0..BANDS {
                if !self.bands[band].is_moving(&self.params) {
                    self.refresh(band);
                }
            }
        } else {
            self.params.skip_all(ctx.frames);
        }

        self.bypass.blend(ctx, state, bypassed);
    }
}

/// Builds [`EqNode`]s.
#[derive(Debug, Default)]
pub struct EqFactory;

impl EqFactory {
    pub const fn new() -> Self {
        Self
    }

    /// Build an EQ that shares the shell's hold-to-solo word.
    pub fn create_with_listen(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
        listen: Arc<AtomicI32>,
    ) -> Box<dyn RtNode> {
        Box::new(EqNode::new_with_listen(
            params,
            sample_rate,
            max_block,
            listen,
        ))
    }
}

impl RtNodeFactory for EqFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode> {
        self.create_with_params(sample_rate, max_block, self.default_params())
    }
}

impl ParamNodeFactory for EqFactory {
    fn params(&self) -> &'static [ParamDef] {
        PARAMS
    }

    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Box<dyn RtNode> {
        Box::new(EqNode::new(params, sample_rate, max_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{level_db_at, render_mono, render_stereo, sine, Harness};

    const SR: f64 = 48_000.0;

    fn harness() -> Harness {
        Harness::new(&EqFactory::new(), SR, 512)
    }

    /// Measured gain in dB at `hz`, through the node, after the ramps settle.
    fn measured_db(h: &mut Harness, hz: f64) -> f64 {
        let input = sine(hz, 32_768, SR, 0.25);
        let out = render_mono(h, &input);
        // Drop the first 8192 frames: the 10 ms ramp plus the filters' own
        // settling time at low frequencies.
        level_db_at(&out[8_192..], hz, SR) - level_db_at(&input[8_192..], hz, SR)
    }

    #[test]
    fn a_fresh_eq_is_bit_transparent() {
        let mut h = harness();
        let input: Vec<[f32; 2]> = (0..1_024)
            .map(|n| [(n as f32 * 0.023).sin() * 0.6, (n as f32 * 0.041).sin()])
            .collect();
        assert_eq!(
            render_stereo(&mut h, &input),
            input,
            "every band at 0 dB with the highpass off must change nothing"
        );
    }

    #[test]
    fn the_audio_path_matches_the_curve_the_ui_will_draw() {
        // The claim that makes `magnitude_db_at` usable as a display: it is not
        // a second model of the EQ, it is the same coefficients evaluated.
        let mut h = harness();
        h.set("low.gain", 6.0);
        h.set("bell1.freq", 800.0);
        h.set("bell1.gain", -9.0);
        h.set("bell1.q", 2.0);
        h.set("high.gain", 4.5);

        for hz in [60.0_f64, 200.0, 800.0, 2_000.0, 12_000.0] {
            let predicted = magnitude_db_at(&h.params, hz, SR);
            let measured = measured_db(&mut h, hz);
            assert!(
                (predicted - measured).abs() < 0.1,
                "{hz} Hz: curve says {predicted:.3} dB, the audio did {measured:.3} dB"
            );
        }
    }

    #[test]
    fn a_bell_boosts_exactly_what_it_says_at_its_centre() {
        let mut h = harness();
        h.set("bell1.freq", 1_000.0);
        h.set("bell1.gain", 12.0);
        h.set("bell1.q", 1.0);
        let measured = measured_db(&mut h, 1_000.0);
        assert!((measured - 12.0).abs() < 0.1, "{measured} dB");

        // And leaves the rest alone.
        assert!(measured_db(&mut h, 60.0).abs() < 0.3);
        assert!(measured_db(&mut h, 12_000.0).abs() < 0.3);
    }

    #[test]
    fn the_highpass_slope_switch_really_changes_the_slope() {
        let mut h = harness();
        h.set("hp.enabled", 1.0);
        h.set("hp.freq", 200.0);

        h.set("hp.slope", 0.0);
        let twelve = measured_db(&mut h, 50.0) - measured_db(&mut h, 100.0);
        h.set("hp.slope", 1.0);
        let twenty_four = measured_db(&mut h, 50.0) - measured_db(&mut h, 100.0);

        assert!(
            (twelve + 12.0).abs() < 1.0,
            "12 dB/oct measured {twelve} per octave"
        );
        assert!(
            (twenty_four + 24.0).abs() < 1.5,
            "24 dB/oct measured {twenty_four} per octave"
        );
    }

    #[test]
    fn a_disabled_band_is_out_of_the_chain_entirely() {
        let mut h = harness();
        h.set("bell1.freq", 1_000.0);
        h.set("bell1.gain", 18.0);
        assert!(measured_db(&mut h, 1_000.0) > 17.0);

        h.set("bell1.enabled", 0.0);
        assert!(
            measured_db(&mut h, 1_000.0).abs() < 0.05,
            "a band that is off must not colour anything"
        );
        // ...and the curve agrees.
        assert!(magnitude_db_at(&h.params, 1_000.0, SR).abs() < 1e-6);
    }

    #[test]
    fn the_output_trim_is_a_flat_offset_on_the_whole_curve() {
        let mut h = harness();
        h.set("output", -6.0206);
        for hz in [100.0_f64, 1_000.0, 10_000.0] {
            assert!((measured_db(&mut h, hz) + 6.0206).abs() < 0.05, "{hz} Hz");
        }
    }

    #[test]
    fn a_gain_step_ramps_rather_than_stepping_the_waveform() {
        // The zipper test at 100 Hz, where a bell's gain change is a big
        // amplitude change: the sample-to-sample difference must stay inside
        // what the signal's own slope plus one ramp step can explain.
        let mut h = harness();
        h.set("bell1.freq", 100.0);
        h.set("bell1.q", 0.7);
        let input = sine(100.0, 8_192, SR, 0.5);
        render_mono(&mut h, &input);

        h.set("bell1.gain", MAX_BAND_GAIN_DB);
        let out = render_mono(&mut h, &input);
        let natural = out
            .windows(2)
            .map(|p| (p[1] - p[0]).abs())
            .fold(0.0_f32, f32::max);
        // A 100 Hz sine at amplitude ~4 steps by at most 2*pi*100/48000*4 = 0.053
        // between samples. Anything much beyond that is the EQ, not the signal.
        assert!(
            natural < 0.08,
            "a gain step produced a jump of {natural}, which is a click"
        );
    }

    #[test]
    fn a_bypassed_eq_is_the_input_again() {
        let mut h = harness();
        h.set("bell1.gain", 18.0);
        h.set("bell1.freq", 1_000.0);
        let input: Vec<[f32; 2]> = (0..8_192)
            .map(|n| [(n as f32 * 0.13).sin() * 0.4, (n as f32 * 0.09).sin() * 0.4])
            .collect();
        render_stereo(&mut h, &input);

        h.set("bypass", 1.0);
        render_stereo(&mut h, &input);
        let out = render_stereo(&mut h, &input);
        assert_eq!(out, input);
    }

    #[test]
    fn the_curve_sampler_spans_the_audio_band_logarithmically() {
        let h = harness();
        let curve = magnitude_curve(&h.params, SR, 128);
        assert_eq!(curve.len(), 128);
        assert!((curve[0].0 - f64::from(MIN_FREQ_HZ)).abs() < 1e-6);
        assert!((curve[127].0 - f64::from(MAX_FREQ_HZ)).abs() < 1e-3);
        // Logarithmic: equal ratios, not equal differences.
        let first = curve[1].0 / curve[0].0;
        let last = curve[127].0 / curve[126].0;
        assert!((first - last).abs() < 1e-6, "{first} vs {last}");
        assert!(
            curve.iter().all(|(_, db)| db.abs() < 1e-9),
            "flat at defaults"
        );
    }

    #[test]
    fn every_band_is_addressable_and_its_ids_are_the_ones_a_project_file_saves() {
        let h = harness();
        for id in BAND_IDS {
            assert!(h.params.get_by_id(&format!("{id}.enabled")).is_some());
            assert!(h.params.get_by_id(&format!("{id}.freq")).is_some());
        }
        // Renaming any of these invalidates saved projects.
        assert_eq!(NODE_TYPE_ID, "effects.eq");
        assert_eq!(BAND_IDS, ["hp", "low", "bell1", "bell2", "high"]);
    }
}
