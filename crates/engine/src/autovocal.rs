//! "Auto vocal": measure a recorded vocal (and the beat's key) and return a calibrated
//! insert chain — rider → FET → compressor → de-esser (+ pitch correction, reverb, limiter).
//!
//! Port of the validated research chain (`research/auto_vocal`, branch `auto-vocal`:
//! `av/mix.py`, `results/layer3_chain_config.json`, `results/dynamics_calibration.json`,
//! `results/fx_calibration.json`). The research stages are offline block processors; the
//! app chain is made of the engine's own real-time effects. So each stage is calibrated
//! the same way: the research formula is run on the signal that reaches the stage (what
//! the research stage *would* do there), then the engine effect's one level-setting control
//! is searched (bisection over real renders) until the engine effect does the same amount
//! of work — the same loudest-1 % reduction for the FET stage, the same mean reduction for
//! the slow stage, the same sibilance after the de-esser.
//!
//! Offline only (never on the audio thread). No ML: pitch uses the engine's own analysis.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::catalog;
use crate::model::FxKind;
use crate::music::KeyEstimate;
use crate::pitch::PitchAnalysis;
use crate::processor::{ProcessCtx, Processor};

// ------------------------------------------------------------------ research constants

/// Rider: word-vs-phrase evening `s` and phrase-vs-take nudge `μ` (dynamics_calibration.json).
pub const RIDER_STRENGTH: f64 = 0.5;
pub const RIDER_MU: f64 = 0.2;
pub const RIDER_MAX_BOOST_DB: f64 = 5.0;
pub const RIDER_MAX_CUT_DB: f64 = 6.0;
/// Fast stage: ~4 dB of reduction on the loudest 1 % of 1 ms blocks, 4:1.
pub const FET_PEAK_GR_DB: f64 = 4.0;
/// Slow stage: ~3 dB mean static reduction over sung 10 ms blocks, 2.5:1, 10/250 ms.
pub const OPTO_AVG_GR_DB: f64 = 3.0;
pub const OPTO_RATIO: f64 = 2.5;
/// Median p95 sibilance of the MUSDB train pro vocals (HF ≥ 5 kHz vs the rest, dB).
pub const SIB_TARGET_DB: f64 = -6.045;
/// Median pro vocal loudness relative to the beat, LU (layer3_targets.json).
pub const VOCAL_MINUS_BEAT_LU: f64 = -1.21;
/// Reverb: small plate, T60 1.2 s, send −18 dB wet/dry energy (fx_calibration.json).
pub const REVERB_T60_S: f64 = 1.2;
pub const REVERB_SEND_DB: f64 = -18.0;
/// Below this share of sung time on the beat's scale, correction snaps chromatically.
pub const SCALE_GATE: f64 = 0.75;
pub const LIMIT_CEILING_DBTP: f64 = -1.0;

// ------------------------------------------------------------------ plan DTOs

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    /// Sung vocal, gentle pitch correction.
    #[default]
    Natural,
    /// Rap / hard tune: faster rider windows, fast retune.
    Hard,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedFx {
    #[serde(rename = "type")]
    pub kind: FxKind,
    /// Full parameter set for the effect (catalog ids → values).
    pub params: BTreeMap<String, f64>,
    /// Effect dry/wet (`Fx::mix`, 0..1).
    pub mix: f64,
    /// `false`: the stage is proposed but should be added bypassed (or skipped).
    pub enabled: bool,
    /// Why these settings, in one line (for a tooltip).
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    pub root: u8,
    pub minor: bool,
    pub name: String,
    pub confidence: f32,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VocalAnalysis {
    pub duration_s: f64,
    pub sample_rate: f32,
    pub peak_dbfs: f64,
    pub true_peak_dbtp: f64,
    /// Integrated loudness (BS.1770, gated).
    pub lufs: f64,
    /// Typical sung word level (K-weighted, 200 ms): the rider's target.
    pub word_level_db: f64,
    /// Noise floor the rider's confidence is measured against.
    pub noise_floor_db: f64,
    /// Share of the take that is confident vocal.
    pub sung_share: f64,
    /// p95 of the high-band/low-band ratio (≥ 5 kHz vs below) on loud 5 ms frames, dB.
    pub sibilance_db: f64,
    pub key: Option<KeyInfo>,
    /// Share of voiced pitch frames on the beat's scale (None without key or pitch).
    pub scale_share: Option<f64>,
    /// Median distance of the voiced pitch from the nearest semitone, cents.
    pub pitch_off_cents: Option<f64>,
    pub beat_lufs: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageStats {
    /// Rider gain on confident frames, p5 / p50 / p95 dB.
    pub rider_gain_db: [f64; 3],
    /// FET stage: reduction on the loudest 1 % of sung 1 ms blocks (research target / engine).
    pub fet_p99_gr_db: [f64; 2],
    /// Slow stage: mean reduction over sung 10 ms blocks (research target / engine).
    pub comp_mean_gr_db: [f64; 2],
    /// Sibilance p95 after the de-esser (research target / engine), dB.
    pub sibilance_after_db: [f64; 2],
    /// Evenness: 400 ms K-weighted loudness IQR on the take's vocal windows, before → after.
    pub iqr400_db: [f64; 2],
    pub lufs_after: f64,
    pub true_peak_after_dbtp: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoVocalPlan {
    pub version: u32,
    pub style: Style,
    pub analysis: VocalAnalysis,
    /// Insert effects in order, first = first in the chain.
    pub chain: Vec<PlannedFx>,
    /// Suggested vocal level change (track fader / clip gain) to sit 1.2 LU under the beat.
    pub track_gain_db: Option<f64>,
    pub stages: StageStats,
    pub warnings: Vec<String>,
    /// Set by [`AutoVocalPlan::baked`]: the rider stage, taken out of `chain`, to be rendered
    /// into the clip's audio (`Engine::bake_rider` with these params, its output trim
    /// included) instead of running live on the insert. `None`: the rider is in `chain`.
    pub rider_bake: Option<PlannedFx>,
}

impl AutoVocalPlan {
    /// The same plan with the rider moved out of the insert chain into [`Self::rider_bake`].
    /// The calibration stays valid: every later stage was calibrated on the rider's offline
    /// render (long look-ahead, latency removed, output trim applied), which is exactly the
    /// audio the bake writes; only the pitch correction, which is not part of the level
    /// calibration, now hears the levelled take instead of the raw one.
    pub fn baked(mut self) -> Self {
        if let Some(i) = self.chain.iter().position(|f| f.kind == FxKind::Rider) {
            self.rider_bake = Some(self.chain.remove(i));
        }
        self
    }
}

/// What the plan is computed from. `vocal` is mono at `sr`.
pub struct PlanInput<'a> {
    pub vocal: &'a [f32],
    pub sr: f32,
    pub style: Style,
    /// The beat's key (engine key estimator over the beat), if any.
    pub key: Option<KeyEstimate>,
    /// Pitch analysis of the vocal (engine's cached YIN/RMVPE analysis), if available.
    pub pitch: Option<&'a PitchAnalysis>,
    /// Mono beat audio and its sample rate, for the level suggestion, if any.
    pub beat: Option<(&'a [f32], f32)>,
}

// ------------------------------------------------------------------ small DSP helpers (f64)

/// numpy-style percentile (linear interpolation). NaN on empty input.
pub fn percentile(v: &[f64], q: f64) -> f64 {
    let mut s: Vec<f64> = v.iter().copied().filter(|x| !x.is_nan()).collect();
    if s.is_empty() {
        return f64::NAN;
    }
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pos = (q / 100.0).clamp(0.0, 1.0) * (s.len() - 1) as f64;
    let i = pos.floor() as usize;
    let j = (i + 1).min(s.len() - 1);
    s[i] + (s[j] - s[i]) * (pos - i as f64)
}

#[derive(Clone, Copy, Default)]
struct Bq {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Bq {
    fn rbj(kind: &str, sr: f64, f: f64, q: f64, g_db: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * f / sr;
        let (sw, cw) = w.sin_cos();
        let alpha = sw / (2.0 * q);
        let a = 10f64.powf(g_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            "hp" => ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            "lp" => ((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            _ => {
                let s = 2.0 * a.sqrt() * alpha;
                (a * ((a + 1.0) + (a - 1.0) * cw + s), -2.0 * a * ((a - 1.0) + (a + 1.0) * cw), a * ((a + 1.0) + (a - 1.0) * cw - s), (a + 1.0) - (a - 1.0) * cw + s, 2.0 * ((a - 1.0) - (a + 1.0) * cw), (a + 1.0) - (a - 1.0) * cw - s)
            }
        };
        Self { b: [b0 / a0, b1 / a0, b2 / a0], a: [a1 / a0, a2 / a0], z: [0.0; 2] }
    }
    #[inline]
    fn tick(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// BS.1770 K-weighting as pyloudnorm applies it (f64).
pub fn k_weight(x: &[f32], sr: f32) -> Vec<f64> {
    let mut sh = Bq::rbj("hs", sr as f64, 1500.0, std::f64::consts::FRAC_1_SQRT_2, 4.0);
    let mut hp = Bq::rbj("hp", sr as f64, 38.0, 0.5, 0.0);
    x.iter().map(|&v| hp.tick(sh.tick(v as f64))).collect()
}

/// 4th-order Butterworth high-pass at 5 kHz (scipy `butter(4, 5000, "highpass")` as sos).
fn hp5k(x: &[f64], sr: f32) -> Vec<f64> {
    let mut a = Bq::rbj("hp", sr as f64, 5000.0, 0.541_196_100_146_197, 0.0);
    let mut b = Bq::rbj("hp", sr as f64, 5000.0, 1.306_562_964_876_376_5, 0.0);
    x.iter().map(|&v| b.tick(a.tick(v))).collect()
}

/// `scipy.ndimage.uniform_filter1d(x, w)` (mode "reflect"), read at `start, start+hop, …`.
pub fn uniform_at(x: &[f64], w: usize, start: usize, hop: usize) -> Vec<f64> {
    let n = x.len();
    if n == 0 {
        return vec![];
    }
    let w = w.max(1);
    let pad = w + 1;
    let at = |i: isize| -> f64 {
        let mut i = i;
        let n = n as isize;
        // half-sample symmetric reflection, repeated if the window is longer than the signal
        loop {
            if i < 0 {
                i = -i - 1;
            } else if i >= n {
                i = 2 * n - i - 1;
            } else {
                return x[i as usize];
            }
        }
    };
    let mut pre = Vec::with_capacity(n + 2 * pad + 1);
    pre.push(0.0);
    let mut acc = 0.0;
    for i in -(pad as isize)..(n + pad) as isize {
        acc += at(i);
        pre.push(acc);
    }
    let lo_off = (w / 2) as isize;
    let mut out = vec![];
    let mut i = start;
    while i < n {
        let a = i as isize - lo_off + pad as isize;
        let b = a + w as isize;
        out.push((pre[b as usize] - pre[a as usize]) / w as f64);
        i += hop;
    }
    out
}

fn db10(p: f64) -> f64 {
    10.0 * (p + 1e-14).log10()
}

// ------------------------------------------------------------------ measurements

/// Rider analysis of a take: (typical word level T, noise floor, loud end p95, confident share).
pub fn rider_levels(x: &[f32], sr: f32, style: Style) -> (f64, f64, f64, f64) {
    let hop = crate::rider::hop(sr);
    let k: Vec<f64> = k_weight(x, sr).iter().map(|v| v * v).collect();
    let pk: Vec<f64> = uniform_at(&k, hop, hop / 2, hop).iter().map(|v| v + 1e-14).collect();
    let wf = if style == Style::Hard { 11 } else { 20 };
    let pf = uniform_at(&pk, wf, 0, 1);
    let lf: Vec<f64> = pf.iter().map(|&p| db10(p)).collect();
    let top = percentile(&lf, 95.0);
    let floor = percentile(&lf, 10.0).min(top - 35.0);
    let conf: Vec<f64> = lf
        .iter()
        .map(|&l| {
            let u = ((l - floor - 6.0) / 8.0).clamp(0.0, 1.0);
            if l > top - 30.0 {
                u * u * (3.0 - 2.0 * u)
            } else {
                0.0
            }
        })
        .collect();
    let confident: Vec<f64> = lf.iter().zip(&conf).filter(|(_, c)| **c > 0.5).map(|(l, _)| *l).collect();
    let t = if confident.len() >= 20 { percentile(&confident, 50.0) } else { top - 6.0 };
    (t, floor, top, confident.len() as f64 / lf.len().max(1) as f64)
}

fn block_db(x: &[f64], n: usize, peak: bool) -> Vec<f64> {
    x.chunks_exact(n.max(1))
        .map(|c| if peak { 20.0 * (c.iter().fold(0f64, |m, v| m.max(v.abs())) + 1e-9).log10() } else { db10(c.iter().map(|v| v * v).sum::<f64>() / c.len() as f64) })
        .collect()
}

fn static_gr(e: f64, thr: f64, ratio: f64, knee: f64) -> f64 {
    let over = e - thr;
    if over > knee / 2.0 {
        over * (1.0 - 1.0 / ratio)
    } else if over > -knee / 2.0 {
        (1.0 - 1.0 / ratio) * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else {
        0.0
    }
}

fn smooth_gain(g: &[f64], n: usize, sr: f32, att_ms: f64, rel_ms: f64) -> Vec<f64> {
    let bt = n as f64 / sr as f64;
    let (aa, ar) = ((-bt / (att_ms / 1000.0)).exp(), (-bt / (rel_ms / 1000.0)).exp());
    let mut s = 0.0;
    g.iter()
        .map(|&v| {
            let a = if v < s { aa } else { ar };
            s = a * s + (1.0 - a) * v;
            s
        })
        .collect()
}

/// Research fast stage on `x`: (static threshold dB, p99 reduction over sung 1 ms blocks, sung mask).
fn research_fet(x: &[f64], sr: f32) -> (f64, f64, Vec<bool>) {
    let n = ((sr as f64 * 0.001) as usize).max(1);
    let e = block_db(x, n, true);
    let p95 = percentile(&e, 95.0);
    let sung: Vec<bool> = e.iter().map(|&v| v > p95 - 30.0).collect();
    let es: Vec<f64> = e.iter().zip(&sung).filter(|(_, s)| **s).map(|(v, _)| *v).collect();
    let thr = percentile(&es, 99.0) - FET_PEAK_GR_DB / (1.0 - 1.0 / 4.0);
    let g: Vec<f64> = e.iter().map(|&v| -static_gr(v, thr, 4.0, 3.0)).collect();
    let g = smooth_gain(&g, n, sr, 1.0, 60.0);
    let gs: Vec<f64> = g.iter().zip(&sung).filter(|(_, s)| **s).map(|(v, _)| -v).collect();
    (thr, percentile(&gs, 99.0), sung)
}

/// Research slow stage on `x`: (threshold, mean reduction over sung 10 ms blocks, sung mask).
fn research_opto(x: &[f64], sr: f32) -> (f64, f64, Vec<bool>) {
    let n = ((sr as f64 * 0.01) as usize).max(1);
    let e = block_db(x, n, false);
    let p95 = percentile(&e, 95.0);
    let sung: Vec<bool> = e.iter().map(|&v| v > p95 - 25.0).collect();
    let es: Vec<f64> = e.iter().zip(&sung).filter(|(_, s)| **s).map(|(v, _)| *v).collect();
    let (mut lo, mut hi) = (percentile(&es, 1.0) - 20.0, percentile(&es, 99.0));
    let mut thr = hi;
    for _ in 0..30 {
        thr = (lo + hi) / 2.0;
        let m = es.iter().map(|&v| static_gr(v, thr, OPTO_RATIO, 6.0)).sum::<f64>() / es.len().max(1) as f64;
        if m > OPTO_AVG_GR_DB {
            lo = thr;
        } else {
            hi = thr;
        }
    }
    let g: Vec<f64> = e.iter().map(|&v| -static_gr(v, thr, OPTO_RATIO, 6.0)).collect();
    let g = smooth_gain(&g, n, sr, 10.0, 250.0);
    let gs: Vec<f64> = g.iter().zip(&sung).filter(|(_, s)| **s).map(|(v, _)| -v).collect();
    (thr, gs.iter().sum::<f64>() / gs.len().max(1) as f64, sung)
}

/// Sibilance p95 (dB) as the research de-esser measures it: 5 ms frames, HF ≥ 5 kHz vs the
/// rest, over frames within 40 dB of the loud end.
pub fn sibilance_p95(x: &[f64], sr: f32) -> f64 {
    let hi = hp5k(x, sr);
    let n = ((sr as f64 * 0.005) as usize).max(1);
    let lo: Vec<f64> = x.iter().zip(&hi).map(|(a, b)| a - b).collect();
    let eh = block_db(&hi, n, false);
    let el = block_db(&lo, n, false);
    let p95 = percentile(&el, 95.0);
    let r: Vec<f64> = eh.iter().zip(&el).filter(|(_, l)| **l > p95 - 40.0).map(|(h, l)| h - l).collect();
    percentile(&r, 95.0)
}

/// Research de-esser threshold on `x` (dB of high/low band ratio).
fn research_deess_thr(x: &[f64], sr: f32) -> f64 {
    let hi = hp5k(x, sr);
    let lo: Vec<f64> = x.iter().zip(&hi).map(|(a, b)| a - b).collect();
    let n = ((sr as f64 * 0.005) as usize).max(1);
    let eh = block_db(&hi, n, false);
    let el = block_db(&lo, n, false);
    let p95l = percentile(&el, 95.0);
    let rl: Vec<f64> = eh.iter().zip(&el).filter(|(_, l)| **l > p95l - 40.0).map(|(h, l)| h - l).collect();
    let p95 = if rl.is_empty() { 0.0 } else { percentile(&rl, 95.0) };
    p95 - (p95 - SIB_TARGET_DB).max(0.0) - 3.0
}

/// Research de-esser on `x`: the output's sibilance p95 (what the engine de-esser must reach).
fn research_deess_sib(x: &[f64], sr: f32) -> f64 {
    let hi = hp5k(x, sr);
    let lo: Vec<f64> = x.iter().zip(&hi).map(|(a, b)| a - b).collect();
    let n = ((sr as f64 * 0.005) as usize).max(1);
    let eh = block_db(&hi, n, false);
    let el = block_db(&lo, n, false);
    let r: Vec<f64> = eh.iter().zip(&el).map(|(h, l)| h - l).collect();
    let p95l = percentile(&el, 95.0);
    let loud: Vec<bool> = el.iter().map(|&v| v > p95l - 40.0).collect();
    let rl: Vec<f64> = r.iter().zip(&loud).filter(|(_, l)| **l).map(|(v, _)| *v).collect();
    let p95 = if rl.is_empty() { 0.0 } else { percentile(&rl, 95.0) };
    let over = p95 - SIB_TARGET_DB;
    let thr = p95 - over.max(0.0) - 3.0;
    let g: Vec<f64> = r.iter().zip(&loud).map(|(&v, &l)| if l && v > thr { (-(v - thr)).max(-12.0) } else { 0.0 }).collect();
    let g = smooth_gain(&g, n, sr, 1.0, 40.0);
    let y: Vec<f64> = (0..x.len()).map(|i| lo[i] + hi[i] * 10f64.powf(g.get(i / n).copied().unwrap_or(*g.last().unwrap_or(&0.0)) / 20.0)).collect();
    sibilance_p95(&y, sr)
}

fn apply_block_gain(x: &[f64], g: &[f64], n: usize) -> Vec<f64> {
    let last = g.last().copied().unwrap_or(0.0);
    x.iter().enumerate().map(|(i, &v)| v * 10f64.powf(g.get(i / n).copied().unwrap_or(last) / 20.0)).collect()
}

/// The research dynamics stages themselves (offline block processors, `av/mix.py`
/// `fet` / `opto` / `deess`), as a reference for tests and the parity example:
/// returns the signal after the fast stage, the slow stage and the de-esser.
pub fn research_stages(xr: &[f32], sr: f32) -> [Vec<f32>; 3] {
    let x = to64(xr);
    // fast
    let n1 = ((sr as f64 * 0.001) as usize).max(1);
    let (thr, _, _) = research_fet(&x, sr);
    let e = block_db(&x, n1, true);
    let g: Vec<f64> = e.iter().map(|&v| -static_gr(v, thr, 4.0, 3.0)).collect();
    let y1 = apply_block_gain(&x, &smooth_gain(&g, n1, sr, 1.0, 60.0), n1);
    // slow
    let n10 = ((sr as f64 * 0.01) as usize).max(1);
    let (thr, _, _) = research_opto(&y1, sr);
    let e = block_db(&y1, n10, false);
    let g: Vec<f64> = e.iter().map(|&v| -static_gr(v, thr, OPTO_RATIO, 6.0)).collect();
    let y2 = apply_block_gain(&y1, &smooth_gain(&g, n10, sr, 10.0, 250.0), n10);
    // de-esser
    let hi = hp5k(&y2, sr);
    let lo: Vec<f64> = y2.iter().zip(&hi).map(|(a, b)| a - b).collect();
    let n5 = ((sr as f64 * 0.005) as usize).max(1);
    let (eh, el) = (block_db(&hi, n5, false), block_db(&lo, n5, false));
    let r: Vec<f64> = eh.iter().zip(&el).map(|(h, l)| h - l).collect();
    let p95l = percentile(&el, 95.0);
    let loud: Vec<bool> = el.iter().map(|&v| v > p95l - 40.0).collect();
    let rl: Vec<f64> = r.iter().zip(&loud).filter(|(_, l)| **l).map(|(v, _)| *v).collect();
    let p95 = if rl.is_empty() { 0.0 } else { percentile(&rl, 95.0) };
    let thr = p95 - (p95 - SIB_TARGET_DB).max(0.0) - 3.0;
    let g: Vec<f64> = r.iter().zip(&loud).map(|(&v, &l)| if l && v > thr { (-(v - thr)).max(-12.0) } else { 0.0 }).collect();
    let hg = apply_block_gain(&hi, &smooth_gain(&g, n5, sr, 1.0, 40.0), n5);
    let y3: Vec<f64> = lo.iter().zip(&hg).map(|(a, b)| a + b).collect();
    let f = |v: &[f64]| v.iter().map(|&x| x as f32).collect::<Vec<f32>>();
    [f(&y1), f(&y2), f(&y3)]
}

/// Integrated loudness, ITU-R BS.1770 (mono, 400 ms blocks, 75 % overlap, both gates).
pub fn integrated_lufs(x: &[f32], sr: f32) -> f64 {
    let k: Vec<f64> = k_weight(x, sr).iter().map(|v| v * v).collect();
    let (w, h) = ((sr * 0.4) as usize, (sr * 0.1) as usize);
    if k.len() < w || h == 0 {
        return -70.0;
    }
    let mut pre = vec![0.0; k.len() + 1];
    for i in 0..k.len() {
        pre[i + 1] = pre[i] + k[i];
    }
    let z: Vec<f64> = (0..=(k.len() - w) / h).map(|j| (pre[j * h + w] - pre[j * h]) / w as f64).collect();
    let l = |z: f64| -0.691 + 10.0 * z.max(1e-20).log10();
    let abs: Vec<f64> = z.iter().copied().filter(|&v| l(v) > -70.0).collect();
    if abs.is_empty() {
        return -70.0;
    }
    let rel = l(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
    let g: Vec<f64> = abs.into_iter().filter(|&v| l(v) > rel).collect();
    l(g.iter().sum::<f64>() / g.len().max(1) as f64)
}

/// Approximate true peak (4× oversampling, windowed sinc), dBTP.
pub fn true_peak_db(x: &[f32]) -> f64 {
    const HALF: isize = 8;
    let mut taps = [[0f64; 2 * HALF as usize]; 3];
    for (p, t) in taps.iter_mut().enumerate() {
        let frac = (p + 1) as f64 / 4.0;
        for (k, v) in t.iter_mut().enumerate() {
            let d = k as f64 - (HALF - 1) as f64 - frac;
            let sinc = if d.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d) };
            let wn = 0.5 + 0.5 * (std::f64::consts::PI * d / HALF as f64).cos();
            *v = sinc * wn;
        }
    }
    let n = x.len() as isize;
    let mut pk = x.iter().fold(0f64, |m, v| m.max(v.abs() as f64));
    for i in 0..n {
        for t in &taps {
            let mut s = 0.0;
            for (k, c) in t.iter().enumerate() {
                let j = i + k as isize - (HALF - 1);
                if j >= 0 && j < n {
                    s += c * x[j as usize] as f64;
                }
            }
            pk = pk.max(s.abs());
        }
    }
    20.0 * (pk + 1e-12).log10()
}

// ------------------------------------------------------------------ engine renders

/// Renders mono `x` through one built-in effect (dual mono), latency removed.
pub fn render(kind: FxKind, params: &BTreeMap<String, f64>, x: &[f32], sr: f32) -> Vec<f32> {
    let mut p = crate::dsp::create(kind).expect("built-in effect");
    render_with(p.as_mut(), kind, params, x, sr)
}

/// Like [`render`], with a processor the caller keeps (to read its state afterwards).
pub fn render_with(p: &mut dyn Processor, kind: FxKind, params: &BTreeMap<String, f64>, x: &[f32], sr: f32) -> Vec<f32> {
    render_stereo_with(p, kind, params, x, x, sr).0
}

/// Renders stereo `l`/`r` (equal lengths) through one built-in effect exactly as [`render`]
/// does: 512-frame blocks, the effect's reported latency rendered past the end and dropped
/// from the front, so the output is sample-aligned with the input and as long as it.
pub fn render_stereo(kind: FxKind, params: &BTreeMap<String, f64>, l: &[f32], r: &[f32], sr: f32) -> (Vec<f32>, Vec<f32>) {
    let mut p = crate::dsp::create(kind).expect("built-in effect");
    render_stereo_with(p.as_mut(), kind, params, l, r, sr)
}

/// [`render_stereo`] with a processor the caller keeps.
pub fn render_stereo_with(p: &mut dyn Processor, kind: FxKind, params: &BTreeMap<String, f64>, xl: &[f32], xr: &[f32], sr: f32) -> (Vec<f32>, Vec<f32>) {
    let mut fx = catalog::new_fx(1, kind, 120.0);
    for (k, v) in params {
        fx.params.insert(k.clone(), *v);
    }
    let lat = catalog::latency(&fx, sr);
    const BLK: usize = 512;
    p.prepare(sr, BLK);
    p.set_params(&catalog::param_block(&fx));
    let ctx = ProcessCtx { sample_rate: sr, bpm: 120.0, beat: 0.0, playing: true };
    let len = xl.len().min(xr.len());
    let total = len + lat;
    let (mut ol, mut or) = (Vec::with_capacity(total), Vec::with_capacity(total));
    let (mut l, mut r) = (vec![0f32; BLK], vec![0f32; BLK]);
    let mut i = 0;
    while i < total {
        let n = BLK.min(total - i);
        for k in 0..n {
            let j = i + k;
            (l[k], r[k]) = if j < len { (xl[j], xr[j]) } else { (0.0, 0.0) };
        }
        p.process(&mut l[..n], &mut r[..n], &ctx);
        ol.extend_from_slice(&l[..n]);
        or.extend_from_slice(&r[..n]);
        i += n;
    }
    ol.drain(..lat.min(ol.len()));
    or.drain(..lat.min(or.len()));
    (ol, or)
}

/// The rider's output and its per-frame (gain dB, confidence, active) curve.
pub fn rider_curve(params: &BTreeMap<String, f64>, x: &[f32], sr: f32) -> (Vec<f32>, Vec<[f32; 3]>) {
    let mut r = crate::rider::Rider::with_log(x.len() / crate::rider::hop(sr) + 2);
    let y = render_with(&mut r, FxKind::Rider, params, x, sr);
    let log = r.log().to_vec();
    (y, log)
}

fn to64(x: &[f32]) -> Vec<f64> {
    x.iter().map(|&v| v as f64).collect()
}

/// Per-block gain (dB) of an engine render vs its input: energy ratio per block.
fn block_gain_db(x: &[f64], y: &[f32], n: usize) -> Vec<f64> {
    x.chunks_exact(n)
        .zip(y.chunks_exact(n))
        .map(|(a, b)| {
            let ea: f64 = a.iter().map(|v| v * v).sum();
            let eb: f64 = b.iter().map(|&v| v as f64 * v as f64).sum();
            if ea > 1e-18 {
                10.0 * (eb.max(1e-30) / ea).log10()
            } else {
                0.0
            }
        })
        .collect()
}

/// Bisection on a monotone control: `f(v)` increases with `v`; finds `f(v) ≈ want`.
fn bisect(mut lo: f64, mut hi: f64, want: f64, iters: usize, mut f: impl FnMut(f64) -> f64) -> f64 {
    for _ in 0..iters {
        let mid = 0.5 * (lo + hi);
        if f(mid) < want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

fn params(kind: FxKind, kv: &[(&str, f64)]) -> BTreeMap<String, f64> {
    let mut m = catalog::default_params(kind);
    for (k, v) in kv {
        m.insert((*k).to_owned(), *v);
    }
    m
}

/// Rider settings for a measured take.
pub fn rider_params(target_db: f64, floor_db: f64, gate_db: f64, style: Style, long_look: bool) -> BTreeMap<String, f64> {
    params(
        FxKind::Rider,
        &[
            ("amount", RIDER_STRENGTH * 100.0),
            ("phrase", RIDER_MU * 100.0),
            ("up", RIDER_MAX_BOOST_DB),
            ("down", RIDER_MAX_CUT_DB),
            ("speed", if style == Style::Hard { 1.0 } else { 0.0 }),
            ("auto", 0.0),
            ("target", target_db.clamp(-60.0, 0.0)),
            ("floor", floor_db.clamp(-100.0, -20.0)),
            ("gate", gate_db.clamp(-100.0, -10.0)),
            ("look", if long_look { 1.0 } else { 0.0 }),
        ],
    )
}

fn fet_params(input_db: f64) -> BTreeMap<String, f64> {
    // 4:1, attack as slow as the FET goes (research: 1 ms), 60 ms release, unity make-up
    params(FxKind::Fet, &[("input", input_db), ("output", -input_db), ("ratio", 0.0), ("attack", 800.0), ("release", 60.0), ("mix", 100.0)])
}

fn comp_params(thr: f64) -> BTreeMap<String, f64> {
    params(FxKind::Comp, &[("thr", thr), ("ratio", OPTO_RATIO), ("att", 10.0), ("rel", 250.0), ("gain", 0.0), ("det", 1.0)])
}

fn deess_params(thr: f64) -> BTreeMap<String, f64> {
    // split band at 5 kHz, relative (ratio) threshold, up to 12 dB: the research de-esser
    params(FxKind::Deess, &[("thr", thr), ("range", 12.0), ("freq", 5000.0), ("mode", 0.0), ("look", 5.0), ("listen", 0.0), ("detect", 1.0)])
}

/// Stage-by-stage calibration result of the dynamics chain.
pub struct Dynamics {
    pub rider: BTreeMap<String, f64>,
    pub fet: BTreeMap<String, f64>,
    pub comp: BTreeMap<String, f64>,
    pub deess: BTreeMap<String, f64>,
    /// signal after the rider (with its output trim) and after the whole chain
    pub after_rider: Vec<f32>,
    /// the rider's per-frame (gain dB, confidence, active)
    pub rider_curve: Vec<[f32; 3]>,
    pub after: Vec<f32>,
    pub stats: StageStats,
}

/// Measures `x` and calibrates rider → FET → compressor → de-esser on it, stage by stage.
pub fn calibrate_dynamics(x: &[f32], sr: f32, style: Style, long_look: bool) -> Dynamics {
    let (t, floor, top, _) = rider_levels(x, sr, style);
    let mut rider = rider_params(t, floor, top - 30.0, style, long_look);
    let (mut xr, curve) = rider_curve(&rider, x, sr);
    // the FET's threshold is fixed (−10 dBFS after its Input knob, which cannot cut): a take
    // too hot for the knob's range is trimmed at the rider's output first
    let (thr_fet, _, _) = research_fet(&to64(&xr), sr);
    let need = -10.0 - thr_fet;
    if need < 4.0 {
        let trim = (need - 8.0).max(-24.0);
        rider.insert("out".into(), (trim * 10.0).round() / 10.0);
        let g = crate::dsp::db_to_gain(rider["out"] as f32);
        xr.iter_mut().for_each(|v| *v *= g);
    }
    let xr64 = to64(&xr);

    // fast stage: match the research stage's loudest-1 % reduction
    let (_, fet_want, sung1) = research_fet(&xr64, sr);
    let n1 = ((sr as f64 * 0.001) as usize).max(1);
    let fet_p99 = |y: &[f32]| {
        let g = block_gain_db(&xr64, y, n1);
        let gs: Vec<f64> = g.iter().zip(&sung1).filter(|(_, s)| **s).map(|(v, _)| -v).collect();
        percentile(&gs, 99.0)
    };
    let fet_in = if fet_want > 0.05 { bisect(0.0, 36.0, fet_want, 11, |v| fet_p99(&render(FxKind::Fet, &fet_params(v), &xr, sr))) } else { 0.0 };
    let fet = fet_params(fet_in);
    let y1 = render(FxKind::Fet, &fet, &xr, sr);
    let fet_got = fet_p99(&y1);
    let y164 = to64(&y1);

    // slow stage: match the research stage's mean reduction (higher threshold = less work)
    let (_, comp_want, sung10) = research_opto(&y164, sr);
    let n10 = ((sr as f64 * 0.01) as usize).max(1);
    let comp_mean = |y: &[f32]| {
        let g = block_gain_db(&y164, y, n10);
        let gs: Vec<f64> = g.iter().zip(&sung10).filter(|(_, s)| **s).map(|(v, _)| -v).collect();
        gs.iter().sum::<f64>() / gs.len().max(1) as f64
    };
    let thr = bisect(-60.0, 0.0, -comp_want, 11, |v| -comp_mean(&render(FxKind::Comp, &comp_params(v), &y1, sr)));
    let comp = comp_params(thr);
    let y2 = render(FxKind::Comp, &comp, &y1, sr);
    let comp_got = comp_mean(&y2);
    let y264 = to64(&y2);

    // de-esser: reach the research de-esser's sibilance (higher threshold = more sibilance)
    let sib_want = research_deess_sib(&y264, sr);
    let t0 = research_deess_thr(&y264, sr);
    let dthr = bisect((t0 - 12.0).max(-60.0), (t0 + 12.0).min(0.0), sib_want, 9, |v| sibilance_p95(&to64(&render(FxKind::Deess, &deess_params(v), &y2, sr)), sr));
    let deess = deess_params(dthr);
    let y3 = render(FxKind::Deess, &deess, &y2, sr);
    let sib_got = sibilance_p95(&to64(&y3), sr);

    // rider gain percentiles on confident frames
    let conf_g: Vec<f64> = curve.iter().filter(|f| f[1] > 0.5).map(|f| f[0] as f64).collect();
    let r3 = |v: f64| (v * 10.0).round() / 10.0;
    let stats = StageStats {
        rider_gain_db: [r3(percentile(&conf_g, 5.0)), r3(percentile(&conf_g, 50.0)), r3(percentile(&conf_g, 95.0))],
        fet_p99_gr_db: [r3(fet_want), r3(fet_got)],
        comp_mean_gr_db: [r3(comp_want), r3(comp_got)],
        sibilance_after_db: [r3(sib_want), r3(sib_got)],
        ..Default::default()
    };
    Dynamics { rider, fet, comp, deess, after_rider: xr, rider_curve: curve, after: y3, stats }
}

/// Reverb mix (%) that gives a `send_db` wet/dry energy ratio on white noise with the
/// research plate (T60, pre-delay 30 ms, wet 300 Hz – 7 kHz). Measured on the engine's
/// reverb, so it stays right if the reverb's internal levels change.
pub fn reverb_mix_for_send(send_db: f64, t60: f64, sr: f32) -> (BTreeMap<String, f64>, f64) {
    let base = |mix: f64| params(FxKind::Reverb, &[("size", t60), ("pre", 30.0), ("lo", 300.0), ("hi", 7000.0), ("width", 100.0), ("mix", mix), ("tone", 0.0), ("space", 0.0), ("duck", 0.0), ("bass", 1.0)]);
    let n = (sr * 4.0) as usize;
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let x: Vec<f32> = (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.2
        })
        .collect();
    // at mix ≤ 50 % the dry path is at unity and the wet gain is 2·mix
    let y = render(FxKind::Reverb, &base(50.0), &x, sr);
    let skip = (sr * 2.0) as usize; // steady state
    let ex: f64 = x[skip..].iter().map(|&v| v as f64 * v as f64).sum();
    let ew: f64 = x[skip..].iter().zip(&y[skip..]).map(|(&a, &b)| (b as f64 - a as f64).powi(2)).sum();
    let wet_db = 10.0 * (ew / ex.max(1e-30)).log10();
    let mix = (50.0 * 10f64.powf((send_db - wet_db) / 20.0)).clamp(0.0, 50.0);
    let r = |v: f64| (v * 100.0).round() / 100.0;
    (base(r(mix)), wet_db)
}

/// 400 ms K-weighted loudness IQR on the vocal windows of `reference` (research metric m1).
pub fn iqr400(x: &[f32], reference: &[f32], sr: f32) -> f64 {
    let m = metrics::vocal_windows(reference, sr);
    metrics::m1(x, sr, &m).1
}

// ------------------------------------------------------------------ the plan

/// Measures the vocal and returns the calibrated chain. Takes a few seconds per minute of
/// audio (every stage is calibrated on real renders); call it off the UI thread.
pub fn plan(inp: &PlanInput) -> AutoVocalPlan {
    let sr = inp.sr;
    let x = inp.vocal;
    let mut warnings = vec![];
    let long_look = true;
    let (t, floor, _top, sung_share) = rider_levels(x, sr, inp.style);
    let x64 = to64(x);
    let mut analysis = VocalAnalysis {
        duration_s: x.len() as f64 / sr as f64,
        sample_rate: sr,
        peak_dbfs: 20.0 * (x.iter().fold(0f32, |m, v| m.max(v.abs())) as f64 + 1e-12).log10(),
        true_peak_dbtp: true_peak_db(x),
        lufs: integrated_lufs(x, sr),
        word_level_db: t,
        noise_floor_db: floor,
        sung_share,
        sibilance_db: sibilance_p95(&x64, sr),
        ..Default::default()
    };
    if x.len() < (sr * 2.0) as usize || sung_share < 0.05 {
        warnings.push("too little sung audio for a reliable analysis".into());
    }
    if analysis.peak_dbfs > -0.3 {
        warnings.push("the take clips (peaks at 0 dBFS); levelling cannot undo clipping".into());
    }

    let dynm = calibrate_dynamics(x, sr, inp.style, long_look);
    let mut stats = dynm.stats.clone();

    // pitch correction: key from the beat, scale gate on the sung pitch
    let mut tune_enabled = false;
    let mut tune_note = "no key detected in the beat: pitch correction left off".to_owned();
    let mut tune = params(FxKind::Tune, &[]);
    if let Some(k) = &inp.key {
        analysis.key = Some(KeyInfo { root: k.root, minor: k.minor, name: crate::music::key_name(k.root, k.minor), confidence: k.confidence });
        let scale = if k.minor { 2u32 } else { 1u32 };
        let mask = crate::pitch::scale_mask(scale);
        let mut chosen = scale as f64;
        if let Some(p) = inp.pitch {
            let (mut on, mut tot) = (0usize, 0usize);
            let mut offs = vec![];
            for (&m, &c) in p.curve.iter().zip(&p.confidence) {
                if m.is_nan() || c < 0.5 {
                    continue;
                }
                let m = m as f64 - k.tuning_cents as f64 / 100.0;
                let nearest = m.round();
                offs.push(((m - nearest) * 100.0).abs());
                let pc = ((nearest as i64 - k.root as i64).rem_euclid(12)) as u16;
                tot += 1;
                if mask & (1 << pc) != 0 {
                    on += 1;
                }
            }
            if tot > 50 {
                let share = on as f64 / tot as f64;
                analysis.scale_share = Some(share);
                analysis.pitch_off_cents = Some(percentile(&offs, 50.0));
                if share < SCALE_GATE {
                    chosen = 0.0;
                }
            }
        }
        let (retune, humanize, flex, vibrato) = if inp.style == Style::Hard { (0.0, 0.0, 0.0, 20.0) } else { (40.0, 30.0, 40.0, 60.0) };
        tune = params(FxKind::Tune, &[("key", k.root as f64), ("scale", chosen), ("retune", retune), ("humanize", humanize), ("flex", flex), ("vibrato", vibrato)]);
        tune_enabled = true;
        tune_note = if chosen == 0.0 {
            format!("{} from the beat, but only {:.0} % of the singing is on that scale: notes snap chromatically", crate::music::key_name(k.root, k.minor), 100.0 * analysis.scale_share.unwrap_or(0.0))
        } else {
            format!("{} from the beat (confidence {:.2})", crate::music::key_name(k.root, k.minor), k.confidence)
        };
        if k.confidence < 0.1 {
            warnings.push("the beat's key is uncertain; check the pitch correction key".into());
        }
    }

    let (reverb, _) = reverb_mix_for_send(REVERB_SEND_DB, REVERB_T60_S, sr);
    let limiter = params(FxKind::Limit, &[("gain", 0.0), ("ceil", LIMIT_CEILING_DBTP), ("rel", 80.0), ("auto", 0.0), ("look", 5.0), ("link", 100.0), ("out", 0.0)]);
    let wet = render(FxKind::Reverb, &reverb, &dynm.after, sr);
    let limited = render(FxKind::Limit, &limiter, &wet, sr);
    stats.iqr400_db = [iqr400(x, x, sr), iqr400(&dynm.after, x, sr)];
    stats.lufs_after = integrated_lufs(&limited, sr);
    stats.true_peak_after_dbtp = true_peak_db(&limited);
    let track_gain_db = inp.beat.map(|(b, bsr)| {
        let lb = integrated_lufs(b, bsr);
        analysis.beat_lufs = Some(lb);
        lb + VOCAL_MINUS_BEAT_LU - stats.lufs_after
    });

    let fx = |kind, params, enabled, note: String| PlannedFx { kind, params, mix: 1.0, enabled, note };
    let r = &dynm.rider;
    let chain = vec![
        fx(FxKind::Tune, tune, tune_enabled, tune_note),
        fx(FxKind::Rider, dynm.rider.clone(), true, format!("target {:.1} dB, noise floor {:.1} dB; gain p5/p50/p95 {:.1}/{:.1}/{:.1} dB", r["target"], r["floor"], stats.rider_gain_db[0], stats.rider_gain_db[1], stats.rider_gain_db[2])),
        fx(FxKind::Fet, dynm.fet, true, format!("fast 4:1 peak stage, {:.1} dB on the loudest 1 %", stats.fet_p99_gr_db[1])),
        fx(FxKind::Comp, dynm.comp, true, format!("slow 2.5:1 stage, {:.1} dB mean reduction", stats.comp_mean_gr_db[1])),
        fx(FxKind::Deess, dynm.deess, true, format!("sibilance {:.1} → {:.1} dB (pro median {SIB_TARGET_DB:.1})", analysis.sibilance_db, stats.sibilance_after_db[1])),
        fx(FxKind::Reverb, reverb, true, format!("small plate, T60 {REVERB_T60_S} s, {REVERB_SEND_DB} dB send")),
        fx(FxKind::Limit, limiter, true, format!("true-peak ceiling {LIMIT_CEILING_DBTP} dBTP")),
    ];
    AutoVocalPlan { version: 1, style: inp.style, analysis, chain, track_gain_db, stages: stats, warnings, rider_bake: None }
}

// ------------------------------------------------------------------ research metrics

/// The research levelling metrics (`av/leveling.py`), for tests and the parity example.
pub mod metrics {
    use super::{k_weight, percentile, uniform_at};

    fn pw(x: &[f32], sr: f32, win_s: f64, hop_s: f64) -> Vec<f64> {
        let k: Vec<f64> = k_weight(x, sr).iter().map(|v| v * v).collect();
        let hop = (hop_s * sr as f64) as usize;
        uniform_at(&k, ((win_s * sr as f64) as usize).max(1), hop / 2, hop).iter().map(|v| v + 1e-14).collect()
    }

    /// 400 ms windows (100 ms hop) within 20 dB of the take's loud end.
    pub fn vocal_windows(reference: &[f32], sr: f32) -> Vec<bool> {
        let lr: Vec<f64> = pw(reference, sr, 0.4, 0.1).iter().map(|p| 10.0 * p.log10()).collect();
        let p95 = percentile(&lr, 95.0);
        lr.iter().map(|&v| v > p95 - 20.0).collect()
    }

    /// m1: 400 ms loudness on `mask`: (median, IQR, P90 − P10).
    pub fn m1(x: &[f32], sr: f32, mask: &[bool]) -> (f64, f64, f64) {
        let lv: Vec<f64> = pw(x, sr, 0.4, 0.1).iter().map(|p| 10.0 * p.log10()).collect();
        let v: Vec<f64> = lv.iter().zip(mask).filter(|(_, m)| **m).map(|(v, _)| *v).collect();
        (percentile(&v, 50.0), percentile(&v, 75.0) - percentile(&v, 25.0), percentile(&v, 90.0) - percentile(&v, 10.0))
    }

    /// Energy segments ("words") of the reference, in 5 ms frames.
    pub fn words(reference: &[f32], sr: f32) -> Vec<(usize, usize)> {
        let lv: Vec<f64> = pw(reference, sr, 0.005, 0.005).iter().map(|p| 10.0 * p.log10()).collect();
        let p95 = percentile(&lv, 95.0);
        let mut act: Vec<bool> = lv.iter().map(|&v| v > p95 - 20.0).collect();
        act.extend([false; 9]);
        let (mut segs, mut a, mut gap, mut last): (Vec<(usize, usize)>, Option<usize>, usize, usize) = (vec![], None, 0, 0);
        for (i, v) in act.into_iter().enumerate() {
            if v {
                a = Some(a.unwrap_or(i));
                gap = 0;
                last = i;
            } else if let Some(s) = a {
                gap += 1;
                if gap >= 8 {
                    if last - s >= 16 {
                        segs.push((s, last + 1));
                    }
                    a = None;
                }
            }
        }
        segs
    }

    /// m2: per-word level spread: (IQR, P90 − P10, word count).
    pub fn m2(x: &[f32], sr: f32, segs: &[(usize, usize)]) -> (f64, f64, usize) {
        let p = pw(x, sr, 0.005, 0.005);
        let lv: Vec<f64> = segs.iter().filter(|(_, b)| *b <= p.len()).map(|&(a, b)| 10.0 * (p[a..b].iter().sum::<f64>() / (b - a) as f64).log10()).collect();
        if lv.len() < 4 {
            return (f64::NAN, f64::NAN, lv.len());
        }
        (percentile(&lv, 75.0) - percentile(&lv, 25.0), percentile(&lv, 90.0) - percentile(&lv, 10.0), lv.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 44100.0;

    fn noise(seed: u32) -> impl FnMut() -> f32 {
        let mut s = seed.max(1);
        move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32 - 0.5) * 2.0
        }
    }

    /// A synthetic take: sung syllables (harmonic, gliding pitch) whose level wanders by
    /// phrase and by word, "s" noise bursts, breaths and a room-noise bed.
    fn take(secs: f32) -> Vec<f32> {
        let n = (secs * SR) as usize;
        let mut nz = noise(3);
        let mut hiss = noise(11);
        let mut x = vec![0f32; n];
        let (mut ph, mut t0) = (0f32, 0usize);
        let mut k = 0;
        while t0 < n {
            let len = (0.28 + 0.17 * ((k * 7 % 5) as f32)) * SR;
            let lvl = -16.0 - 7.0 * ((k as f32 * 0.9).sin() + 1.0) * 0.5 - if (k / 6) % 2 == 1 { 6.0 } else { 0.0 };
            let a = 10f32.powf(lvl / 20.0);
            let f0 = 196.0 * 2f32.powf(((k * 5 % 7) as f32) / 12.0);
            for i in 0..len as usize {
                if t0 + i >= n {
                    break;
                }
                let t = i as f32 / len;
                let env = (t / 0.08).min(1.0) * ((1.0 - t) / 0.15).min(1.0);
                ph += 2.0 * std::f32::consts::PI * f0 * (1.0 + 0.004 * (i as f32 / SR * 5.5 * std::f32::consts::TAU).sin()) / SR;
                let v = ph.sin() + 0.6 * (2.0 * ph).sin() + 0.35 * (3.0 * ph).sin() + 0.2 * (5.0 * ph).sin();
                x[t0 + i] += a * env * v * 0.5;
                // an "s" at the start of every third syllable
                if k % 3 == 0 && i < (0.09 * SR) as usize {
                    x[t0 + i] += a * 0.5 * hiss() * if i % 2 == 0 { 1.0 } else { -1.0 };
                }
            }
            t0 += len as usize + (0.06 * SR) as usize;
            // a breath every phrase
            if k % 6 == 5 {
                for i in 0..(0.35 * SR) as usize {
                    if t0 + i < n {
                        x[t0 + i] += 0.004 * nz();
                    }
                }
                t0 += (0.45 * SR) as usize;
            }
            k += 1;
        }
        for v in x.iter_mut() {
            *v += 2e-4 * nz();
        }
        x
    }

    #[test]
    fn plan_calibrates_every_stage() {
        let x = take(14.0);
        let key = crate::music::KeyEstimate { root: 7, minor: false, confidence: 0.5, candidates: vec![], chroma: [0.0; 12], tuning_cents: 0.0 };
        let beat: Vec<f32> = take(14.0).iter().map(|v| v * 2.0).collect();
        let plan = plan(&PlanInput { vocal: &x, sr: SR, style: Style::Natural, key: Some(key), pitch: None, beat: Some((&beat, SR)) });
        let kinds: Vec<FxKind> = plan.chain.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [FxKind::Tune, FxKind::Rider, FxKind::Fet, FxKind::Comp, FxKind::Deess, FxKind::Reverb, FxKind::Limit]);
        let s = &plan.stages;
        assert!((s.fet_p99_gr_db[0] - s.fet_p99_gr_db[1]).abs() < 0.5, "FET {:?}", s.fet_p99_gr_db);
        assert!((s.comp_mean_gr_db[0] - s.comp_mean_gr_db[1]).abs() < 0.3, "comp {:?}", s.comp_mean_gr_db);
        assert!((s.sibilance_after_db[0] - s.sibilance_after_db[1]).abs() < 1.0, "sibilance {:?}", s.sibilance_after_db);
        assert!(s.iqr400_db[1] < s.iqr400_db[0] - 1.0, "not evened: {:?}", s.iqr400_db);
        assert!(s.true_peak_after_dbtp <= -0.8, "true peak {}", s.true_peak_after_dbtp);
        assert!(plan.chain[0].enabled && plan.chain[0].params["key"] == 7.0 && plan.chain[0].params["scale"] == 1.0);
        let r = &plan.chain[1].params;
        assert_eq!(r["auto"], 0.0);
        assert!(r["target"] > -40.0 && r["target"] < -10.0 && r["gate"] < r["target"]);
        // every planned value is inside its parameter's range
        for f in &plan.chain {
            for d in catalog::params(f.kind) {
                let v = f.params[d.id];
                assert!(v >= d.min && v <= d.max, "{:?}.{} = {v} outside {}..{}", f.kind, d.id, d.min, d.max);
            }
        }
        // a beat 6 dB louder than the vocal take: the vocal goes up to sit 1.2 LU under it
        let g = plan.track_gain_db.unwrap();
        assert!(g > 2.0, "track gain {g}");
        let j = serde_json::to_value(&plan).unwrap();
        assert!(j["chain"][1]["type"] == "rider" && j["stages"]["fetP99GrDb"].is_array() && j["analysis"]["wordLevelDb"].is_number());
    }

    #[test]
    fn no_key_leaves_tune_off_and_scale_gate_trips() {
        let x = take(6.0);
        let p = plan(&PlanInput { vocal: &x, sr: SR, style: Style::Hard, key: None, pitch: None, beat: None });
        assert!(!p.chain[0].enabled);
        assert!(p.track_gain_db.is_none());
        assert_eq!(p.chain[1].params["speed"], 1.0);
        // a melody off the beat's scale snaps chromatically
        let mut curve = vec![f32::NAN; 400];
        for (i, c) in curve.iter_mut().enumerate().take(300) {
            *c = [61.0, 63.0, 66.0, 68.0, 70.0][i % 5]; // only black keys: none is in C major
        }
        let pa = PitchAnalysis { rate: 200.0, curve, levels: vec![0.1; 400], confidence: vec![0.9; 400], notes: vec![], backend: "yin".into() };
        let key = crate::music::KeyEstimate { root: 0, minor: false, confidence: 0.5, candidates: vec![], chroma: [0.0; 12], tuning_cents: 0.0 };
        let p = plan(&PlanInput { vocal: &x, sr: SR, style: Style::Natural, key: Some(key), pitch: Some(&pa), beat: None });
        assert_eq!(p.chain[0].params["scale"], 0.0);
        assert!(p.analysis.scale_share.unwrap() < 0.75);
    }

    #[test]
    fn reverb_send_is_calibrated_on_the_engine_reverb() {
        let (p, wet_db) = reverb_mix_for_send(REVERB_SEND_DB, REVERB_T60_S, SR);
        let mix = p["mix"];
        assert!(mix > 0.0 && mix < 50.0, "mix {mix}");
        // re-measure: wet/dry energy on steady noise at the chosen mix
        let mut nz = noise(5);
        let x: Vec<f32> = (0..(SR * 4.0) as usize).map(|_| 0.1 * nz()).collect();
        let y = render(FxKind::Reverb, &p, &x, SR);
        let k = (SR * 2.0) as usize;
        let ex: f64 = x[k..].iter().map(|&v| (v as f64).powi(2)).sum();
        let ew: f64 = x[k..].iter().zip(&y[k..]).map(|(&a, &b)| (b as f64 - a as f64).powi(2)).sum();
        let got = 10.0 * (ew / ex).log10();
        assert!((got - REVERB_SEND_DB).abs() < 1.0, "send {got:.2} dB (wet at mix 50: {wet_db:.2})");
        println!("reverb mix for {REVERB_SEND_DB} dB send: {mix} %");
    }

    #[test]
    fn relative_deesser_ducks_s_not_vowels() {
        let mut nz = noise(9);
        let n = (SR * 0.5) as usize;
        // vowel: 300 Hz harmonics; "s": white noise (mostly above 5 kHz in energy)
        let vowel: Vec<f32> = (0..n).map(|i| 0.3 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SR).sin()).collect();
        let s: Vec<f32> = (0..n).map(|_| 0.1 * nz()).collect();
        let p = deess_params(-8.0);
        let rv = render(FxKind::Deess, &p, &vowel, SR);
        let rs = render(FxKind::Deess, &p, &s, SR);
        // the same "s" 20 dB quieter: a relative threshold reduces it just as much
        let quiet: Vec<f32> = s.iter().map(|v| v * 0.1).collect();
        let rq = render(FxKind::Deess, &p, &quiet, SR);
        let e = |v: &[f32]| v[n / 2..].iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
        let dv = 10.0 * (e(&rv) / e(&vowel)).log10();
        let ds = 10.0 * (e(&rs) / e(&s)).log10();
        let dq = 10.0 * (e(&rq) / e(&quiet)).log10();
        assert!(dv.abs() < 0.2, "vowel changed by {dv:.2} dB");
        // (like the research de-esser, low = input − 4th-order high-pass, so part of the
        // band around the split frequency stays in the low band)
        assert!(ds < -0.6, "s reduced by only {ds:.2} dB");
        assert!((ds - dq).abs() < 0.1, "level dependent: {ds:.2} vs {dq:.2} dB");
    }

    #[test]
    fn rms_compressor_follows_the_mean_square() {
        // a −10 dBFS-peak sine has −13 dBFS RMS: 7 dB over −20 at 2:1 → 3.5 dB reduction
        let n = (SR * 1.0) as usize;
        let x: Vec<f32> = (0..n).map(|i| 0.316_227_8 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SR).sin()).collect();
        let p = params(FxKind::Comp, &[("thr", -20.0), ("ratio", 2.0), ("att", 5.0), ("rel", 100.0), ("gain", 0.0), ("det", 1.0)]);
        let y = render(FxKind::Comp, &p, &x, SR);
        let e = |v: &[f32]| v[n / 2..].iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
        let gr = 10.0 * (e(&y) / e(&x)).log10();
        assert!((gr + 3.5).abs() < 0.4, "RMS-mode reduction {gr:.2} dB");
    }

    #[test]
    fn metrics_match_numpy_conventions() {
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 25.0), 1.75);
        // uniform_filter1d([1, 2, 3, 4, 5], 3) with "reflect" = [1.33, 2, 3, 4, 4.67]
        let u = uniform_at(&[1.0, 2.0, 3.0, 4.0, 5.0], 3, 0, 1);
        let want = [4.0 / 3.0, 2.0, 3.0, 4.0, 14.0 / 3.0];
        assert!(u.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-12), "{u:?}");
        // even window: origin at w/2 (offsets −2..+1 for w = 4)
        let u = uniform_at(&[1.0, 2.0, 3.0, 4.0, 5.0], 4, 2, 1);
        assert!((u[0] - 2.5).abs() < 1e-12, "{u:?}");
    }
}
