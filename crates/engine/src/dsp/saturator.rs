//! Character saturator: five analog-style voicings around one dynamic nonlinearity.
//!
//! Signal path per channel (design notes and measurements: docs/saturator.md):
//!
//! ```text
//! in ─ up 2×/4× ─┬─ drive gain ─ low cut (+thump) ─ tone tilt ─ pre-emphasis ─ shaper ─ de-emphasis
//!                │     ─ voicing shelf ─ DC block ─ high cut ─ auto gain ─┐
//!                └──────────────────── dry ──────────────────────────── mix ─ down ─ output
//! ```
//!
//! The shaper is not a static curve: a bias that follows the signal envelope makes the
//! asymmetry (even harmonics) bloom on loud notes and fall back after them, a one-pole
//! loop around the curve gives it a little memory (transients pass harder than the
//! sustain), and per-voicing pre/de-emphasis decides which frequencies hit the curve
//! hardest (the transformer drives its lows into the "core"). Aliasing is held down by
//! polyphase IIR half-band oversampling (minimum latency) plus first-order
//! antiderivative antialiasing applied to the distortion part only, so the linear part
//! stays flat and undelayed. No allocation after `prepare`.

use std::sync::OnceLock;

use super::{db_to_gain, flush, Biquad, FilterType, Smooth};
use crate::processor::{ParamBlock, ProcessCtx, Processor};

/// Parameter order (= catalog `SAT`).
pub mod idx {
    pub const STYLE: usize = 0;
    pub const DRIVE: usize = 1;
    pub const PUNISH: usize = 2;
    pub const LOWCUT: usize = 3;
    pub const THUMP: usize = 4;
    pub const TONE: usize = 5;
    pub const HIGHCUT: usize = 6;
    pub const STEEP: usize = 7;
    pub const AUTO: usize = 8;
    pub const MIX: usize = 9;
    pub const OUT: usize = 10;
    pub const HQ: usize = 11;
}

/// Voicings, in parameter order.
pub const STYLES: [&str; 5] = ["Taśma", "Konsola", "Transformator", "Trioda", "Pentoda"];
pub const TAPE: usize = 0;
pub const CONSOLE: usize = 1;
pub const TRANSFORMER: usize = 2;
pub const TRIODE: usize = 3;
pub const PENTODE: usize = 4;

/// Drive above this many dB is the top of the knob; Punish adds 20 dB on top.
pub const DRIVE_MAX_DB: f32 = 36.0;
pub const PUNISH_DB: f32 = 20.0;
/// High cut at (or above) this frequency is off.
pub const HIGHCUT_OFF: f32 = 20000.0;
/// Low cut at (or below) this frequency is off (unless Thump is up).
pub const LOWCUT_OFF: f32 = 10.0;

/// Drive knob position 0..1 → gain into the stage, dB. Quadratic: the first half of the
/// knob is the gentle warmth range (≤ 9 dB), the second half goes to grit.
#[inline]
pub fn drive_db(pos: f32) -> f32 {
    let p = pos.clamp(0.0, 1.0);
    DRIVE_MAX_DB * p * p
}

/// Inverse of [`drive_db`].
#[inline]
pub fn drive_pos(db: f32) -> f32 {
    (db / DRIVE_MAX_DB).clamp(0.0, 1.0).sqrt()
}

/// "Grit": 0 up to half the knob, then a smooth rise to 1 at the top.
#[inline]
fn grit(pos: f32) -> f32 {
    let t = ((pos - 0.5) / 0.5).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ------------------------------------------------------------------ settings

/// Everything the saturator core needs, in natural units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub style: usize,
    /// Drive knob 0..1 (see [`drive_db`]).
    pub drive: f32,
    pub punish: bool,
    /// Hz; ≤ [`LOWCUT_OFF`] = off.
    pub lowcut: f32,
    /// Resonance at the low cut, 0..1.
    pub thump: f32,
    /// Pre-saturation tilt, −1..1.
    pub tone: f32,
    /// Hz; ≥ [`HIGHCUT_OFF`] = off.
    pub highcut: f32,
    /// High cut ~30 dB/oct instead of 6 dB/oct.
    pub steep: bool,
    /// Level-matched output.
    pub auto: bool,
    /// 0..1
    pub mix: f32,
    pub out_db: f32,
    /// 4× oversampling instead of 2×.
    pub hq: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            style: TAPE,
            drive: 0.4,
            punish: false,
            lowcut: LOWCUT_OFF,
            thump: 0.0,
            tone: 0.0,
            highcut: HIGHCUT_OFF,
            steep: false,
            auto: true,
            mix: 1.0,
            out_db: 0.0,
            hq: false,
        }
    }
}

impl Settings {
    pub fn from_block(p: &ParamBlock) -> Self {
        let v = p.values;
        Self {
            style: (v[idx::STYLE].round().max(0.0) as usize).min(STYLES.len() - 1),
            drive: (v[idx::DRIVE] / 100.0).clamp(0.0, 1.0),
            punish: v[idx::PUNISH] >= 0.5,
            lowcut: v[idx::LOWCUT].clamp(LOWCUT_OFF, 1000.0),
            thump: (v[idx::THUMP] / 100.0).clamp(0.0, 1.0),
            tone: (v[idx::TONE] / 100.0).clamp(-1.0, 1.0),
            highcut: v[idx::HIGHCUT].clamp(500.0, HIGHCUT_OFF),
            steep: v[idx::STEEP] >= 0.5,
            auto: v[idx::AUTO] >= 0.5,
            mix: (v[idx::MIX] / 100.0).clamp(0.0, 1.0),
            out_db: v[idx::OUT].clamp(-36.0, 24.0),
            hq: v[idx::HQ] >= 0.5,
        }
    }
    fn gain_db(&self) -> f32 {
        drive_db(self.drive) + if self.punish { PUNISH_DB } else { 0.0 }
    }
}

/// Brings `sat` parameters saved before the character saturator (drive 0–36 dB, mix,
/// out) to the current set. Same gain into the stage, tape voicing, auto gain off (the
/// old effect only half-compensated its drive, so Output keeps its meaning). Files that
/// already have the new set are untouched. Returns whether anything changed.
pub fn migrate_params(p: &mut std::collections::BTreeMap<String, f64>) -> bool {
    if p.contains_key("style") {
        return false;
    }
    let old_drive = p.get("drive").copied().unwrap_or(6.0).clamp(0.0, 36.0);
    p.insert("drive".into(), (drive_pos(old_drive as f32) * 100.0) as f64);
    p.insert("style".into(), TAPE as f64);
    p.insert("auto".into(), 0.0);
    // the old curve also lost ~half its drive in dB at the output
    let out = p.get("out").copied().unwrap_or(0.0);
    p.insert("out".into(), (out - old_drive * 0.5).clamp(-24.0, 12.0));
    true
}

// ------------------------------------------------------------------ voicings

const NV: usize = 17;
mod v {
    /// Level at the curve per unit of input (headroom of the stage).
    pub const K: usize = 0;
    /// Curve blend: symmetric tanh, asymmetric (tube) tanh, cubic clipper.
    pub const WT: usize = 1;
    pub const WA: usize = 2;
    pub const WC: usize = 3;
    /// Negative-half steepness of the asymmetric curve (it clips 1/ASYM earlier).
    pub const ASYM: usize = 4;
    /// Static bias and bias added per unit of envelope (both in curve units).
    pub const BIAS: usize = 5;
    pub const BENV: usize = 6;
    /// Envelope attack / release, ms.
    pub const ATT: usize = 7;
    pub const REL: usize = 8;
    /// Pre-emphasis shelves (undone after the curve): low shelf dB @ Hz, high shelf dB @ Hz.
    pub const PLO_DB: usize = 9;
    pub const PLO_F: usize = 10;
    pub const PHI_DB: usize = 11;
    pub const PHI_F: usize = 12;
    /// Post high shelf that stays (the voicing's top end), dB @ Hz.
    pub const SOFT_DB: usize = 13;
    pub const SOFT_F: usize = 14;
    /// Memory loop: amount 0..0.5 and its corner, Hz.
    pub const MEM: usize = 15;
    pub const MEM_HZ: usize = 16;
}

#[rustfmt::skip]
fn voicing(style: usize) -> [f32; NV] {
    //              K     WT    WA    WC    ASYM  BIAS  BENV  ATT  REL    PLO   @      PHI  @       SOFT  @        MEM   @
    match style {
        // tape preamp: smooth, mostly 2nd+3rd, highs hit the curve harder and come back rounded
        TAPE =>        [0.60, 0.55, 0.45, 0.0,  1.35, 0.06, 0.22, 5.0, 120.0, 1.5,  90.0,  6.0, 3000.0, -1.5, 11000.0, 0.25, 800.0],
        // solid-state console channel: bright, odd, a fast loop that squashes transients
        CONSOLE =>     [0.50, 0.40, 0.0,  0.60, 1.0,  0.02, 0.05, 0.5, 30.0,  0.0,  100.0, 2.5, 5000.0, 0.8,  12000.0, 0.35, 4000.0],
        // germanium / transformer input: the lows drive the core, highs softened
        TRANSFORMER => [0.55, 0.80, 0.20, 0.0,  1.25, 0.05, 0.12, 3.0, 150.0, 10.0, 150.0, 0.0, 4000.0, -2.5, 7000.0,  0.15, 300.0],
        // triode: strongly asymmetric, bias swings with level → 2nd/4th, dense mids
        TRIODE =>      [0.75, 0.0,  1.0,  0.0,  2.0,  0.12, 0.45, 2.0, 80.0,  0.0,  100.0, 1.5, 1500.0, -1.0, 9000.0,  0.10, 1500.0],
        // pentode: symmetric hard knee, odd 3rd/5th, cold and aggressive
        _ =>           [0.90, 0.25, 0.0,  0.75, 1.0,  0.0,  0.01, 1.0, 40.0,  -2.0, 200.0, 4.0, 2000.0, 0.0,  10000.0, 0.05, 2000.0],
    }
}

/// Voicing at a drive knob position: past half the knob the clipper and the envelope bias grow.
fn voicing_at(style: usize, pos: f32) -> [f32; NV] {
    let mut w = voicing(style);
    let g = grit(pos);
    w[v::WC] += 0.5 * g;
    let s = w[v::WT] + w[v::WA] + w[v::WC];
    for i in [v::WT, v::WA, v::WC] {
        w[i] /= s;
    }
    w[v::BENV] *= 1.0 + g;
    w
}

// ------------------------------------------------------------------ curves

const LN2: f64 = std::f64::consts::LN_2;

#[inline]
fn lncosh(u: f64) -> f64 {
    let a = u.abs();
    a + (-2.0 * a).exp().ln_1p() - LN2
}

/// The blended curve and its antiderivative, at fixed weights.
#[derive(Clone, Copy, Default)]
struct Curve {
    wt: f64,
    wa: f64,
    wc: f64,
    k: f64,
}

impl Curve {
    #[inline]
    fn f(&self, u: f64) -> f64 {
        let mut y = 0.0;
        if u >= 0.0 {
            // both tanh curves agree on the positive half
            if self.wt + self.wa > 1e-4 {
                y += (self.wt + self.wa) * u.tanh();
            }
        } else {
            if self.wt > 1e-4 {
                y += self.wt * u.tanh();
            }
            if self.wa > 1e-4 {
                y += self.wa * (self.k * u).tanh() / self.k;
            }
        }
        if self.wc > 1e-4 {
            // cubic soft clipper, unity slope at 0, flat from |u| = 1.5
            let x = u / 1.5;
            y += self.wc * if x.abs() <= 1.0 { 1.5 * (x - x * x * x / 3.0) } else { x.signum() };
        }
        y
    }
    #[inline]
    fn big_f(&self, u: f64) -> f64 {
        let mut y = 0.0;
        if u >= 0.0 {
            if self.wt + self.wa > 1e-4 {
                y += (self.wt + self.wa) * lncosh(u);
            }
        } else {
            if self.wt > 1e-4 {
                y += self.wt * lncosh(u);
            }
            if self.wa > 1e-4 {
                y += self.wa * lncosh(self.k * u) / (self.k * self.k);
            }
        }
        if self.wc > 1e-4 {
            let x = u / 1.5;
            let a = x.abs();
            y += self.wc * 1.5 * if a <= 1.0 { 1.5 * (x * x / 2.0 - x * x * x * x / 12.0) } else { a - 0.375 };
        }
        y
    }
}

/// Shaper state for one channel.
#[derive(Clone, Copy, Default)]
struct Shaper {
    /// previous curve input (ADAA)
    x1: f64,
    env: f32,
    /// memory loop
    z: f32,
    primed: bool,
    /// F(x1), valid for curve generation `gen`
    f1: f64,
    gen: u32,
    /// f(bias) at bias `bq`
    fb: f64,
    bq: f32,
}

/// Per-block shaper constants (rates already at the oversampled rate).
#[derive(Clone, Copy, Default)]
struct ShapeK {
    curve: Curve,
    bias: f32,
    benv: f32,
    ka: f32,
    kr: f32,
    mem: f32,
    km: f32,
    adaa: bool,
    /// bumped whenever `curve` changes (ADAA's cached F(x1) is then stale)
    gen: u32,
}

impl Shaper {
    /// `u` is the signal in curve units; returns curve units, linear part exact.
    #[inline]
    fn tick(&mut self, u: f32, s: &ShapeK) -> f32 {
        let a = u.abs();
        let k = if a > self.env { s.ka } else { s.kr };
        self.env = flush(self.env + (a - self.env) * k);
        let b = (s.bias + s.benv * self.env).min(1.5);
        let uu = u + s.mem * self.z;
        let xin = (uu + b) as f64;
        let c = &s.curve;
        if !self.primed || self.gen != s.gen {
            if !self.primed {
                // the operating point silence leaves (a parked chain wakes up identically)
                self.x1 = s.bias as f64;
            }
            self.f1 = c.big_f(self.x1);
            self.fb = c.f(b as f64);
            self.bq = b;
            self.gen = s.gen;
            self.primed = true;
        }
        // first-order ADAA: the mean of f over the step from the previous input
        let fy = if s.adaa {
            let dx = xin - self.x1;
            let f = c.big_f(xin);
            let y = if dx.abs() > 1e-6 { (f - self.f1) / dx } else { c.f(0.5 * (xin + self.x1)) };
            self.f1 = f;
            y
        } else {
            c.f(xin)
        };
        self.x1 = xin;
        // the operating point moves slowly: refresh f(bias) only when it has moved
        if (b - self.bq).abs() > 2e-5 || (self.env == 0.0 && b != self.bq) {
            self.fb = c.f(b as f64);
            self.bq = b;
        }
        let y = (fy - self.fb) as f32;
        // memory: low-passed error of the curve, fed back to its input
        self.z = flush(self.z + ((y - uu) - self.z) * s.km);
        y
    }
}

// ------------------------------------------------------------------ half-band oversampling

const MAXC: usize = 12;

/// Polyphase IIR half-band (two allpass chains), coefficients by the elliptic design of
/// Valenzuela & Constantinides as used in Laurent de Soras' HIIR.
#[derive(Clone, Copy, Default)]
struct HbCoefs {
    c: [f32; MAXC],
    n: usize,
}

fn hb_design(n: usize, tbw: f64) -> HbCoefs {
    let order = (n * 2 + 1) as f64;
    let pi = std::f64::consts::PI;
    // transition parameters
    let mut k = ((1.0 - tbw * 2.0) * pi / 4.0).tan();
    k *= k;
    let kksqrt = (1.0 - k * k).powf(0.25);
    let e = 0.5 * (1.0 - kksqrt) / (1.0 + kksqrt);
    let e2 = e * e;
    let e4 = e2 * e2;
    let q = e * (1.0 + e4 * (2.0 + e4 * (15.0 + 150.0 * e4)));
    let mut out = HbCoefs { n, ..Default::default() };
    for (i, coeff) in out.c[..n].iter_mut().enumerate() {
        let c = (i + 1) as f64;
        let mut num = 0.0;
        let mut j = 1.0;
        let mut ii = 0.0f64;
        loop {
            let t = q.powf(ii * (ii + 1.0)) * ((ii * 2.0 + 1.0) * c * pi / order).sin() * j;
            num += t;
            j = -j;
            ii += 1.0;
            if t.abs() < 1e-100 || ii > 60.0 {
                break;
            }
        }
        num *= q.powf(0.25);
        let mut den = 0.0;
        let mut j = -1.0;
        let mut ii = 1.0f64;
        loop {
            let t = q.powf(ii * ii) * (ii * 2.0 * c * pi / order).cos() * j;
            den += t;
            j = -j;
            ii += 1.0;
            if t.abs() < 1e-100 || ii > 60.0 {
                break;
            }
        }
        den += 0.5;
        let ww = num / den;
        let wwsq = ww * ww;
        let x = ((1.0 - wwsq * k) * (1.0 - wwsq / k)).sqrt() / (1.0 + wwsq);
        *coeff = ((1.0 - x) / (1.0 + x)) as f32;
    }
    out
}

/// Two allpass chains (even / odd coefficients), each section (a + z⁻¹)/(1 + a z⁻¹) at the low rate.
#[derive(Clone, Copy, Default)]
struct Hb {
    xa: [f32; MAXC / 2],
    ya: [f32; MAXC / 2],
    xb: [f32; MAXC / 2],
    yb: [f32; MAXC / 2],
    prev: f32,
}

impl Hb {
    #[inline]
    fn chain(c: &HbCoefs, start: usize, xs: &mut [f32; MAXC / 2], ys: &mut [f32; MAXC / 2], mut v: f32) -> f32 {
        let mut i = start;
        let mut k = 0;
        while i < c.n {
            let o = c.c[i] * (v - ys[k]) + xs[k];
            xs[k] = v;
            ys[k] = flush(o);
            v = o;
            i += 2;
            k += 1;
        }
        v
    }
    /// One low-rate sample in, two high-rate samples out.
    #[inline]
    fn up(&mut self, c: &HbCoefs, x: f32) -> (f32, f32) {
        (Self::chain(c, 0, &mut self.xa, &mut self.ya, x), Self::chain(c, 1, &mut self.xb, &mut self.yb, x))
    }
    /// Two high-rate samples in, one low-rate sample out.
    #[inline]
    fn down(&mut self, c: &HbCoefs, e: f32, o: f32) -> f32 {
        let a = Self::chain(c, 0, &mut self.xa, &mut self.ya, e);
        let y = 0.5 * (a + self.prev);
        self.prev = Self::chain(c, 1, &mut self.xb, &mut self.yb, o);
        y
    }
}

/// Complex response of a half-band at normalised angular frequency `w` (high rate).
fn hb_response(c: &HbCoefs, w: f64) -> (f64, f64) {
    let ap = |start: usize| {
        // product of (a + z^-2)/(1 + a z^-2)
        let (mut re, mut im) = (1.0f64, 0.0f64);
        let (s2, c2) = (2.0 * w).sin_cos();
        let mut i = start;
        while i < c.n {
            let a = c.c[i] as f64;
            let (nr, ni) = (a + c2, -s2);
            let (dr, di) = (1.0 + a * c2, -a * s2);
            let d = dr * dr + di * di;
            let (hr, hi) = ((nr * dr + ni * di) / d, (ni * dr - nr * di) / d);
            let (r2, i2) = (re * hr - im * hi, re * hi + im * hr);
            re = r2;
            im = i2;
            i += 2;
        }
        (re, im)
    };
    let (ar, ai) = ap(0);
    let (br, bi) = ap(1);
    let (s1, c1) = w.sin_cos();
    // z^-1 * B
    let (zr, zi) = (br * c1 + bi * s1, bi * c1 - br * s1);
    (0.5 * (ar + zr), 0.5 * (ai + zi))
}

/// Group delay of a half-band near DC, in high-rate samples.
fn hb_delay(c: &HbCoefs) -> f64 {
    let w = 2.0 * std::f64::consts::PI * 0.002;
    let (r1, i1) = hb_response(c, w);
    let (r2, i2) = hb_response(c, w * 1.01);
    let (p1, p2) = (i1.atan2(r1), i2.atan2(r2));
    -(p2 - p1) / (w * 0.01)
}

fn coefs() -> &'static (HbCoefs, HbCoefs) {
    static C: OnceLock<(HbCoefs, HbCoefs)> = OnceLock::new();
    // stage 1 (base ↔ 2×): passband to ~0.215 of the high rate, > 100 dB stopband;
    // stage 2 (2× ↔ 4×): the audio band is far below its transition, so 4 sections do
    C.get_or_init(|| (hb_design(12, 0.035), hb_design(4, 0.13)))
}

/// Correction FIR strength per oversampling factor (flat to ±0.2 dB up to 20 kHz).
fn fir_a(os: usize) -> f32 {
    match os {
        1 => 0.0,
        2 => 0.17,
        _ => 0.14,
    }
}

/// Latency in samples at the base rate (rounded) for 2× (`hq` false) or 4× oversampling.
pub fn latency_samples(hq: bool) -> usize {
    latency_exact(hq).round() as usize
}

/// Up + down group delay near DC, base-rate samples.
pub fn latency_exact(hq: bool) -> f64 {
    let (c1, c2) = coefs();
    // each stage: up + down = 2 × half-band delay at its high rate
    let mut d = 2.0 * hb_delay(c1) / 2.0;
    if hq {
        d += 2.0 * hb_delay(c2) / 4.0;
    }
    // ADAA half sample + correction FIR one sample, at the oversampled rate
    d + 1.5 / if hq { 4.0 } else { 2.0 }
}

// ------------------------------------------------------------------ auto gain

const AUTO_STEPS: usize = 21;
/// Reference level of the auto-gain calibration: pink noise at −18 dBFS RMS.
pub const AUTO_REF_DBFS: f32 = -18.0;

/// Deterministic pink noise (Kellet filter on an LCG), RMS-normalised to `dbfs`.
pub fn pink_noise(n: usize, dbfs: f32, seed: u32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(747796405).wrapping_add(2891336453);
    let mut b = [0f32; 7];
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        let w = (s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
        b[0] = 0.99886 * b[0] + w * 0.0555179;
        b[1] = 0.99332 * b[1] + w * 0.0750759;
        b[2] = 0.969 * b[2] + w * 0.153_852;
        b[3] = 0.8665 * b[3] + w * 0.310_485_6;
        b[4] = 0.55000 * b[4] + w * 0.5329522;
        b[5] = -0.7616 * b[5] - w * 0.0168980;
        out.push(b.iter().sum::<f32>() + w * 0.5362);
        b[6] = w * 0.115926;
    }
    let rms = (out.iter().map(|x| x * x).sum::<f32>() / n as f32).sqrt().max(1e-9);
    let g = db_to_gain(dbfs) / rms;
    for x in &mut out {
        *x *= g;
    }
    out
}

/// Auto-gain table: [style][punish][drive step of 5 %] → output gain that level-matches
/// the wet signal to its input on pink noise at −18 dBFS RMS (2×, 48 kHz, 1.5 s after a
/// 0.5 s settle). Generated by `print_auto_table` (run it after changing a voicing).
#[rustfmt::skip]
const AUTO: [[[f32; AUTO_STEPS]; 2]; 5] = [
    [
        [1.10494, 1.09386, 1.06131, 1.00931, 0.94104, 0.86043, 0.77193, 0.68007, 0.58910, 0.50277, 0.42405, 0.35498, 0.29668, 0.24967, 0.21396, 0.18879, 0.17260, 0.16346, 0.14760, 0.13763, 0.13295],
        [0.18851, 0.18773, 0.18543, 0.18182, 0.17716, 0.17181, 0.16613, 0.16050, 0.15523, 0.15056, 0.14664, 0.14366, 0.13961, 0.13593, 0.13343, 0.13181, 0.13078, 0.13012, 0.12976, 0.12952, 0.12948],
    ],
    [
        [1.07114, 1.06018, 1.02796, 0.97646, 0.90873, 0.82863, 0.74045, 0.64857, 0.55708, 0.46957, 0.38887, 0.31701, 0.25519, 0.20395, 0.16333, 0.13300, 0.11184, 0.09779, 0.08875, 0.08306, 0.07968],
        [0.13625, 0.13538, 0.13283, 0.12882, 0.12366, 0.11775, 0.11150, 0.10530, 0.09946, 0.09420, 0.08965, 0.08583, 0.08273, 0.08033, 0.07858, 0.07751, 0.07717, 0.07663, 0.07631, 0.07616, 0.07611],
    ],
    [
        [1.13894, 1.12778, 1.09501, 1.04269, 0.97409, 0.89324, 0.80472, 0.71314, 0.62283, 0.53749, 0.45992, 0.39154, 0.33284, 0.28403, 0.24453, 0.21318, 0.18884, 0.17058, 0.15558, 0.14415, 0.13720],
        [0.21819, 0.21724, 0.21442, 0.20991, 0.20395, 0.19687, 0.18901, 0.18076, 0.17247, 0.16449, 0.15713, 0.15054, 0.14475, 0.13980, 0.13605, 0.13343, 0.13171, 0.13056, 0.12991, 0.12952, 0.12933],
    ],
    [
        [1.13485, 1.12370, 1.09096, 1.03868, 0.97004, 0.88906, 0.80026, 0.70824, 0.61739, 0.53150, 0.45364, 0.38682, 0.33330, 0.29451, 0.27176, 0.26397, 0.21509, 0.17736, 0.15758, 0.14734, 0.14235],
        [0.22788, 0.22718, 0.22513, 0.22192, 0.21781, 0.21314, 0.20821, 0.20189, 0.19175, 0.18091, 0.17243, 0.16545, 0.15917, 0.15375, 0.14928, 0.14574, 0.14304, 0.14106, 0.13968, 0.13885, 0.13856],
    ],
    [
        [1.07642, 1.06551, 1.03346, 0.98225, 0.91494, 0.83544, 0.74807, 0.65727, 0.56721, 0.48162, 0.40352, 0.33524, 0.27857, 0.23474, 0.20332, 0.18211, 0.16837, 0.15973, 0.15446, 0.15134, 0.14956],
        [0.18441, 0.18382, 0.18210, 0.17943, 0.17603, 0.17220, 0.16822, 0.16434, 0.16077, 0.15764, 0.15503, 0.15292, 0.15129, 0.15009, 0.14921, 0.14856, 0.14806, 0.14772, 0.14790, 0.14822, 0.14823],
    ],
];

fn auto_table() -> &'static [[[f32; AUTO_STEPS]; 2]; 5] {
    &AUTO
}

/// Measures the auto-gain table (slow: ~200 renders of 2 s).
pub fn calibrate_auto() -> [[[f32; AUTO_STEPS]; 2]; 5] {
    let sr = 48000.0;
    let noise = pink_noise(96000, AUTO_REF_DBFS, 7);
    let skip = 24000;
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    let rin = rms(&noise[skip..]);
    let mut t = [[[1f32; AUTO_STEPS]; 2]; 5];
    for (style, ts) in t.iter_mut().enumerate() {
        for (pi, tp) in ts.iter_mut().enumerate() {
            for (di, g) in tp.iter_mut().enumerate() {
                let set =
                    Settings { style, drive: di as f32 / (AUTO_STEPS - 1) as f32, punish: pi == 1, auto: false, ..Default::default() };
                let mut core = Core { calibrating: true, ..Default::default() };
                core.set(&set);
                core.prepare(sr, 1);
                let mut x = noise.clone();
                core.process_mono(&mut x);
                *g = (rin / rms(&x[skip..]).max(1e-9)).clamp(1e-3, 10.0);
            }
        }
    }
    t
}

fn auto_gain(style: usize, punish: bool, pos: f32) -> f32 {
    let t = &auto_table()[style.min(4)][punish as usize];
    let x = pos.clamp(0.0, 1.0) * (AUTO_STEPS - 1) as f32;
    let i = (x as usize).min(AUTO_STEPS - 2);
    let f = x - i as f32;
    // interpolate in dB
    let (a, b) = (t[i].ln(), t[i + 1].ln());
    (a + (b - a) * f).exp()
}

// ------------------------------------------------------------------ core

#[derive(Clone, Copy, Default)]
struct OnePole {
    k: f32,
    y: f32,
}

/// Linear filters + shaper state of one channel at the oversampled rate.
#[derive(Clone, Copy, Default)]
struct Chan {
    up1: Hb,
    dn1: Hb,
    up2: Hb,
    dn2: Hb,
    lowcut: Biquad,
    tilt_lo: Biquad,
    tilt_hi: Biquad,
    pre_lo: Biquad,
    pre_hi: Biquad,
    de_lo: Biquad,
    de_hi: Biquad,
    soft: Biquad,
    hc_a: Biquad,
    hc_b: Biquad,
    hc_1: OnePole,
    dc_x: f32,
    dc_y: f32,
    sh: Shaper,
    /// wet correction FIR and the dry path's matching (ADAA + FIR) delay line
    fir: [f32; 2],
    dry: [f32; 3],
}

fn copy_coefs(dst: &mut Biquad, src: &Biquad) {
    dst.b0 = src.b0;
    dst.b1 = src.b1;
    dst.b2 = src.b2;
    dst.a1 = src.a1;
    dst.a2 = src.a2;
}

/// Block-rate constants shared by the channels.
#[derive(Clone, Copy, Default)]
struct Run {
    k: f32,
    inv_k: f32,
    sk: ShapeK,
    /// ADAA's (1 + z⁻¹)/2 is undone by [−a, 1 + 2a, −a] up to 20 kHz; 0 without ADAA
    fir_a: f32,
    lowcut_on: bool,
    tone_on: bool,
    hc_mode: u8, // 0 off, 1 one-pole, 2 steep
    dc_r: f32,
}

impl Chan {
    /// Wet path for one oversampled sample (input already drive-gained).
    #[inline]
    fn wet(&mut self, x: f32, r: &Run) -> f32 {
        let mut v = x;
        if r.lowcut_on {
            v = self.lowcut.tick(v);
        }
        if r.tone_on {
            v = self.tilt_hi.tick(self.tilt_lo.tick(v));
        }
        v = self.pre_hi.tick(self.pre_lo.tick(v));
        let y0 = self.sh.tick(v * r.k, &r.sk) * r.inv_k;
        let y = if r.sk.adaa { (1.0 + 2.0 * r.fir_a) * self.fir[0] - r.fir_a * (y0 + self.fir[1]) } else { y0 };
        self.fir = [y0, self.fir[0]];
        let mut w = self.soft.tick(self.de_hi.tick(self.de_lo.tick(y)));
        // DC block (the bias moves the operating point)
        let hp = w - self.dc_x + r.dc_r * self.dc_y;
        self.dc_x = w;
        self.dc_y = flush(hp);
        w = hp;
        match r.hc_mode {
            1 => {
                self.hc_1.y = flush(self.hc_1.y + (w - self.hc_1.y) * self.hc_1.k);
                w = self.hc_1.y;
            }
            2 => {
                self.hc_1.y = flush(self.hc_1.y + (w - self.hc_1.y) * self.hc_1.k);
                w = self.hc_b.tick(self.hc_a.tick(self.hc_1.y));
            }
            _ => {}
        }
        w
    }
}

/// The saturator for one or two channels; used by the `sat` effect and by Voice's drive.
pub struct Core {
    sr: f32,
    nch: usize,
    os: usize,
    /// Measurement hook: force 1×/2×/4× (1 = no oversampling).
    os_override: Option<usize>,
    /// Measurement hook: antiderivative antialiasing on/off.
    adaa: bool,
    /// building the auto-gain table itself
    calibrating: bool,
    want: Settings,
    applied_hq: bool,
    applied_steep: bool,
    ch: [Chan; 2],
    run: Run,
    // block-rate smoothed state
    vo: [f32; NV],
    eq: [f32; 4],
    eq_done: [f32; 4],
    vo_done: [f32; NV],
    kb: f32,
    // per-sample smoothers (base rate)
    s_gain: Smooth,
    s_mix: Smooth,
    s_comp: Smooth,
    s_out: Smooth,
    duck: f32,
    duck_k: f32,
    pending: bool,
    fresh: bool,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            sr: 48000.0,
            nch: 2,
            os: 2,
            os_override: None,
            adaa: true,
            calibrating: false,
            want: Settings::default(),
            applied_hq: false,
            applied_steep: false,
            ch: [Chan::default(); 2],
            run: Run::default(),
            vo: voicing(TAPE),
            eq: [0.0; 4],
            eq_done: [f32::NAN; 4],
            vo_done: [f32::NAN; NV],
            kb: 0.1,
            s_gain: Smooth::default(),
            s_mix: Smooth::default(),
            s_comp: Smooth::default(),
            s_out: Smooth::default(),
            duck: 1.0,
            duck_k: 0.01,
            pending: false,
            fresh: true,
        }
    }
}

/// Base-rate samples per parameter block.
const BLOCK: usize = 16;

impl Core {
    /// Allocates nothing; computes the oversampling and auto-gain tables on first use.
    pub fn prepare(&mut self, sr: f32, channels: usize) {
        self.sr = sr;
        self.nch = channels.clamp(1, 2);
        let _ = coefs();
        for s in [&mut self.s_gain, &mut self.s_mix, &mut self.s_comp, &mut self.s_out] {
            s.setup(sr, 20.0);
        }
        // block-rate one-pole, ~20 ms
        self.kb = 1.0 - (-(BLOCK as f32) / (sr * 0.02)).exp();
        self.duck_k = 1.0 - (-1.0 / (sr * 0.0015)).exp();
        self.applied_hq = self.want.hq;
        self.applied_steep = self.want.steep;
        self.os = self.os_override.unwrap_or(if self.want.hq { 4 } else { 2 });
        self.fresh = true;
        self.reset();
    }

    pub fn set(&mut self, s: &Settings) {
        self.want = *s;
        if self.fresh {
            self.applied_hq = s.hq;
            self.applied_steep = s.steep;
            self.os = self.os_override.unwrap_or(if s.hq { 4 } else { 2 });
        } else if s.hq != self.applied_hq || s.steep != self.applied_steep {
            self.pending = true;
        }
    }

    /// Measurement hook: force the oversampling factor (1, 2 or 4) and switch ADAA, to
    /// compare against a naive saturator. Call before `prepare`.
    pub fn measurement(&mut self, os: Option<usize>, adaa: bool) {
        self.os_override = os;
        self.adaa = adaa;
    }

    /// Current latency in base-rate samples.
    pub fn latency(&self) -> usize {
        match self.os {
            1 => 0,
            4 => latency_samples(true),
            _ => latency_samples(false),
        }
    }

    pub fn reset(&mut self) {
        for c in &mut self.ch {
            let keep = *c;
            *c = Chan::default();
            // keep coefficients, drop state
            for (d, s) in [
                (&mut c.lowcut, &keep.lowcut),
                (&mut c.tilt_lo, &keep.tilt_lo),
                (&mut c.tilt_hi, &keep.tilt_hi),
                (&mut c.pre_lo, &keep.pre_lo),
                (&mut c.pre_hi, &keep.pre_hi),
                (&mut c.de_lo, &keep.de_lo),
                (&mut c.de_hi, &keep.de_hi),
                (&mut c.soft, &keep.soft),
                (&mut c.hc_a, &keep.hc_a),
                (&mut c.hc_b, &keep.hc_b),
            ] {
                copy_coefs(d, s);
            }
            c.hc_1.k = keep.hc_1.k;
        }
        self.fresh = true;
    }

    fn targets(&self) -> ([f32; NV], [f32; 4], f32, f32, f32, f32) {
        let s = &self.want;
        let vo = voicing_at(s.style, s.drive);
        let thump_q = 0.707 + 3.3 * s.thump;
        let eq = [s.lowcut, thump_q, s.tone * 5.0, s.highcut];
        let gain = db_to_gain(s.gain_db());
        let comp = if s.auto && !self.calibrating { auto_gain(s.style, s.punish, s.drive) } else { 1.0 };
        (vo, eq, gain, s.mix, comp, db_to_gain(s.out_db))
    }

    /// Advance the block-rate smoothing and refresh coefficients that moved.
    fn block_update(&mut self) {
        let (tv, te, ..) = self.targets();
        if self.fresh {
            self.vo = tv;
            self.eq = te;
        } else {
            let k = self.kb;
            for (v, t) in self.vo.iter_mut().zip(tv) {
                *v += (t - *v) * k;
            }
            // frequencies glide in the log domain
            for (i, (e, t)) in self.eq.iter_mut().zip(te).enumerate() {
                if i == 0 || i == 3 {
                    let (a, b) = (e.max(1.0).ln(), t.max(1.0).ln());
                    *e = (a + (b - a) * k).exp();
                } else {
                    *e += (t - *e) * k;
                }
            }
        }
        let close = |a: f32, b: f32| (a - b).abs() <= 1e-4 * a.abs().max(1.0);
        let eq_moved = (0..4).any(|i| !close(self.eq[i], self.eq_done[i]));
        let vo_moved = (0..NV).any(|i| !close(self.vo[i], self.vo_done[i]));
        let fs = self.sr * self.os as f32;
        let mut proto = Biquad::default();
        if eq_moved {
            let [lc, q, tilt, hc] = self.eq;
            self.run.lowcut_on = lc > LOWCUT_OFF + 0.5 || q > 0.72;
            proto.set(FilterType::Highpass, fs, lc, q, 0.0);
            for c in &mut self.ch {
                copy_coefs(&mut c.lowcut, &proto);
            }
            self.run.tone_on = tilt.abs() > 0.01;
            proto.set(FilterType::LowShelf, fs, 800.0, 0.6, -tilt);
            for c in &mut self.ch {
                copy_coefs(&mut c.tilt_lo, &proto);
            }
            proto.set(FilterType::HighShelf, fs, 1200.0, 0.6, tilt);
            for c in &mut self.ch {
                copy_coefs(&mut c.tilt_hi, &proto);
            }
            self.run.hc_mode = if hc >= HIGHCUT_OFF - 1.0 {
                0
            } else if self.applied_steep {
                2
            } else {
                1
            };
            let k1 = 1.0 - (-std::f32::consts::TAU * hc / fs).exp();
            // 5th-order Butterworth when steep (one pole + Q 0.618 + Q 1.618): 30 dB/oct
            let mut pa = Biquad::default();
            let mut pb = Biquad::default();
            pa.set(FilterType::Lowpass, fs, hc, 0.618, 0.0);
            pb.set(FilterType::Lowpass, fs, hc, 1.618, 0.0);
            for c in &mut self.ch {
                c.hc_1.k = k1;
                copy_coefs(&mut c.hc_a, &pa);
                copy_coefs(&mut c.hc_b, &pb);
            }
            self.eq_done = self.eq;
        }
        if vo_moved {
            let w = &self.vo;
            for (f, t, db, fr, sign) in [
                (0usize, FilterType::LowShelf, v::PLO_DB, v::PLO_F, 1.0f32),
                (1, FilterType::HighShelf, v::PHI_DB, v::PHI_F, 1.0),
                (2, FilterType::LowShelf, v::PLO_DB, v::PLO_F, -1.0),
                (3, FilterType::HighShelf, v::PHI_DB, v::PHI_F, -1.0),
                (4, FilterType::HighShelf, v::SOFT_DB, v::SOFT_F, 1.0),
            ] {
                proto.set(t, fs, w[fr], 0.7, sign * w[db]);
                for c in &mut self.ch {
                    let dst = match f {
                        0 => &mut c.pre_lo,
                        1 => &mut c.pre_hi,
                        2 => &mut c.de_lo,
                        3 => &mut c.de_hi,
                        _ => &mut c.soft,
                    };
                    copy_coefs(dst, &proto);
                }
            }
            let tau = |ms: f32| 1.0 - (-1.0 / (fs * ms.max(0.01) / 1000.0)).exp();
            let wsum = (w[v::WT] + w[v::WA] + w[v::WC]).max(1e-6);
            self.run.sk = ShapeK {
                curve: Curve {
                    wt: (w[v::WT] / wsum) as f64,
                    wa: (w[v::WA] / wsum) as f64,
                    wc: (w[v::WC] / wsum) as f64,
                    k: w[v::ASYM].max(1.0) as f64,
                },
                bias: w[v::BIAS],
                benv: w[v::BENV],
                ka: tau(w[v::ATT]),
                kr: tau(w[v::REL]),
                mem: w[v::MEM],
                km: 1.0 - (-std::f32::consts::TAU * w[v::MEM_HZ] / fs).exp(),
                adaa: self.adaa,
                gen: self.run.sk.gen.wrapping_add(1),
            };
            self.run.k = w[v::K];
            self.run.inv_k = 1.0 / w[v::K];
            self.vo_done = self.vo;
        }
        self.run.dc_r = 1.0 - std::f32::consts::TAU * 5.0 / fs;
        self.run.fir_a = fir_a(self.os);
    }

    #[inline]
    fn os_sample(c: &mut Chan, run: &Run, x: f32, g: f32, mix: f32, comp: f32) -> f32 {
        let w = c.wet(x * g, run) * comp;
        // the dry path gets the same linear filtering as the wet one: (1 + z⁻¹)/2 · FIR
        let d = if run.sk.adaa {
            let a = run.fir_a;
            let (c0, c1) = (-0.5 * a, 0.5 * (1.0 + a));
            let d = c0 * (x + c.dry[2]) + c1 * (c.dry[0] + c.dry[1]);
            c.dry = [x, c.dry[0], c.dry[1]];
            d
        } else {
            x
        };
        d + (w - d) * mix
    }

    /// Process up to two channels in place (`r` ignored for a mono core).
    pub fn process(&mut self, l: &mut [f32], mut r: Option<&mut [f32]>) {
        let n = l.len();
        let (c1, c2) = *coefs();
        let mut i0 = 0;
        while i0 < n {
            let i1 = (i0 + BLOCK).min(n);
            // discrete switches (oversampling factor, steep slope) under a short duck
            if self.pending && self.duck < 1e-3 {
                self.pending = false;
                self.applied_hq = self.want.hq;
                self.applied_steep = self.want.steep;
                self.os = self.os_override.unwrap_or(if self.want.hq { 4 } else { 2 });
                for c in &mut self.ch {
                    c.up1 = Hb::default();
                    c.dn1 = Hb::default();
                    c.up2 = Hb::default();
                    c.dn2 = Hb::default();
                    c.hc_a.reset();
                    c.hc_b.reset();
                }
                self.eq_done = [f32::NAN; 4];
                self.vo_done = [f32::NAN; NV];
            }
            self.block_update();
            let (_, _, tg, tm, tc, to) = self.targets();
            if self.fresh {
                self.s_gain.v = tg;
                self.s_mix.v = tm;
                self.s_comp.v = tc;
                self.s_out.v = to;
                self.duck = 1.0;
                self.fresh = false;
            }
            let duck_t = if self.pending { 0.0 } else { 1.0 };
            let run = self.run;
            for i in i0..i1 {
                let g = self.s_gain.next(tg);
                let m = self.s_mix.next(tm);
                let cp = self.s_comp.next(tc);
                let o = self.s_out.next(to);
                self.duck += (duck_t - self.duck) * self.duck_k;
                let og = o * self.duck;
                for chn in 0..self.nch {
                    let x = if chn == 0 {
                        l[i]
                    } else {
                        match r.as_deref() {
                            Some(r) => r[i],
                            None => break,
                        }
                    };
                    let c = &mut self.ch[chn];
                    let y = match self.os {
                        1 => Self::os_sample(c, &run, x, g, m, cp),
                        4 => {
                            let (h0, h1) = c.up1.up(&c1, x);
                            let (q0, q1) = c.up2.up(&c2, h0);
                            let (q2, q3) = c.up2.up(&c2, h1);
                            let (w0, w1) = (Self::os_sample(c, &run, q0, g, m, cp), Self::os_sample(c, &run, q1, g, m, cp));
                            let (w2, w3) = (Self::os_sample(c, &run, q2, g, m, cp), Self::os_sample(c, &run, q3, g, m, cp));
                            let a = c.dn2.down(&c2, w0, w1);
                            let b = c.dn2.down(&c2, w2, w3);
                            c.dn1.down(&c1, a, b)
                        }
                        _ => {
                            let (h0, h1) = c.up1.up(&c1, x);
                            let (w0, w1) = (Self::os_sample(c, &run, h0, g, m, cp), Self::os_sample(c, &run, h1, g, m, cp));
                            c.dn1.down(&c1, w0, w1)
                        }
                    };
                    let y = if y.is_finite() { y * og } else { 0.0 };
                    if chn == 0 {
                        l[i] = y;
                    } else if let Some(r) = r.as_deref_mut() {
                        r[i] = y;
                    }
                }
            }
            i0 = i1;
        }
    }

    pub fn process_mono(&mut self, x: &mut [f32]) {
        self.process(x, None);
    }

    /// Bring the state to what silence leaves: zeros, settled smoothers and switches.
    pub fn park(&mut self) {
        if self.pending {
            self.pending = false;
            self.applied_hq = self.want.hq;
            self.applied_steep = self.want.steep;
            self.os = self.os_override.unwrap_or(if self.want.hq { 4 } else { 2 });
            self.eq_done = [f32::NAN; 4];
            self.vo_done = [f32::NAN; NV];
        }
        self.reset();
    }
}

// ------------------------------------------------------------------ the `sat` effect

/// The built-in `sat` insert.
#[derive(Default)]
pub struct Saturator {
    core: Core,
    sr: f32,
}

impl Processor for Saturator {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        self.core.prepare(sr, 2);
    }
    fn set_params(&mut self, p: &ParamBlock) {
        self.core.set(&Settings::from_block(p));
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        self.core.process(l, Some(r));
    }
    fn reset(&mut self) {
        self.core.reset();
    }
    fn park(&mut self) {
        self.core.park();
    }
    fn tail_samples(&self) -> Option<usize> {
        // resonant low cut, envelope release and DC blocker settle well within 0.3 s
        Some((self.sr.max(8000.0) * 0.3) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, amp: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (std::f32::consts::TAU * f * i as f32 / sr).sin() * amp).collect()
    }

    fn run(set: Settings, x: &[f32], sr: f32) -> Vec<f32> {
        let mut c = Core::default();
        c.prepare(sr, 1);
        c.set(&set);
        let mut y = x.to_vec();
        for ch in y.chunks_mut(256) {
            c.process_mono(ch);
        }
        y
    }

    fn bin(y: &[f32], f: f32, sr: f32) -> f32 {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, &s) in y.iter().enumerate() {
            let ph = std::f64::consts::TAU * f as f64 * i as f64 / sr as f64;
            re += s as f64 * ph.cos();
            im += s as f64 * ph.sin();
        }
        ((re * re + im * im).sqrt() * 2.0 / y.len() as f64) as f32
    }

    #[test]
    fn halfband_rejects_images_and_passes_audio() {
        let (c1, c2) = coefs();
        let db = |c: &HbCoefs, f: f64| {
            let (r, i) = hb_response(c, std::f64::consts::TAU * f);
            20.0 * (r * r + i * i).sqrt().log10()
        };
        // stage 1 at 96 kHz: 20 kHz passes, 28 kHz+ (images of 20 kHz) is gone
        assert!(db(c1, 20000.0 / 96000.0).abs() < 0.01, "{}", db(c1, 20000.0 / 96000.0));
        assert!(db(c1, 28000.0 / 96000.0) < -100.0, "{}", db(c1, 28000.0 / 96000.0));
        assert!(db(c2, 24000.0 / 192000.0).abs() < 0.01);
        assert!(db(c2, 72000.0 / 192000.0) < -70.0, "{}", db(c2, 72000.0 / 192000.0));
    }

    #[test]
    fn small_signals_pass_flat_and_on_time() {
        let sr = 48000.0;
        for hq in [false, true] {
            let set = Settings { drive: 0.0, auto: false, style: CONSOLE, hq, mix: 0.0, ..Default::default() };
            let lat = latency_exact(hq);
            for f in [100.0f32, 1000.0, 10000.0, 18000.0] {
                let x = sine(f, 0.001, sr, 24000);
                let y = run(set, &x, sr);
                let g = bin(&y[12000..], f, sr) / bin(&x[12000..], f, sr);
                assert!((20.0 * g.log10()).abs() < 0.3, "hq {hq} {f} Hz: {} dB", 20.0 * g.log10());
            }
            // the reported latency lines a low tone up within a sample
            let x = sine(200.0, 0.001, sr, 24000);
            let y = run(set, &x, sr);
            let d = lat.round() as usize;
            let err: f32 = (12000..20000).map(|i| (y[i + d] - x[i]).abs()).fold(0.0, f32::max);
            assert!(err < 0.001 * 0.06, "hq {hq}: misaligned by {err}");
        }
    }

    #[test]
    fn triode_is_even_pentode_is_odd() {
        let sr = 48000.0;
        let x = sine(1000.0, 0.5, sr, 72000);
        let h = |style| {
            let y = run(Settings { style, drive: 0.6, auto: false, ..Default::default() }, &x, sr);
            let s = &y[24000..72000];
            let h1 = bin(s, 1000.0, sr);
            (bin(s, 2000.0, sr) / h1, bin(s, 3000.0, sr) / h1)
        };
        let (t2, t3) = h(TRIODE);
        let (p2, p3) = h(PENTODE);
        assert!(t2 > t3, "triode H2 {t2} H3 {t3}");
        assert!(p3 > 10.0 * p2, "pentode H2 {p2} H3 {p3}");
    }

    #[test]
    fn hq_aliases_less_than_2x() {
        let sr = 48000.0;
        let x = sine(5000.0, 0.5, sr, 96000);
        let alias = |hq| {
            let y = run(Settings { style: PENTODE, drive: 1.0, hq, ..Default::default() }, &x, sr);
            let s = &y[48000..96000];
            let mut off = 0.0;
            for k in 1..24 {
                let f = k as f32 * 1000.0;
                if k % 5 != 0 {
                    off += bin(s, f, sr).powi(2);
                }
            }
            off.sqrt() / bin(s, 5000.0, sr)
        };
        let (a2, a4) = (alias(false), alias(true));
        assert!(a4 < a2, "2× {a2} 4× {a4}");
        assert!(a4 < 0.01, "4× aliasing {a4}");
    }

    #[test]
    fn auto_gain_matches_level_over_the_drive_range() {
        let sr = 48000.0;
        let x = pink_noise(48000, AUTO_REF_DBFS, 3);
        let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        let rin = rms(&x[8000..]);
        for (style, name) in STYLES.iter().enumerate() {
            for d in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let y = run(Settings { style, drive: d, ..Default::default() }, &x, sr);
                let db = 20.0 * (rms(&y[8000..]) / rin).log10();
                assert!(db.abs() < 1.0, "{name} drive {d}: {db:.2} dB");
            }
        }
    }

    /// `cargo test -p wave-engine --release --lib print_auto_table -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn print_auto_table() {
        let t = calibrate_auto();
        let mut o = String::from(
            "const AUTO_TABLE: [[[f32; AUTO_STEPS]; 2]; 5] = [
",
        );
        for st in &t {
            o += "    [
";
            for p in st {
                o += "        [";
                o += &p.iter().map(|g| format!("{g:.5}")).collect::<Vec<_>>().join(", ");
                o += "],
";
            }
            o += "    ],
";
        }
        o += "];";
        println!("{o}");
    }

    #[test]
    fn mix_zero_is_dry_and_silence_stays_silent() {
        let sr = 48000.0;
        let x = sine(440.0, 0.5, sr, 24000);
        let y = run(Settings { drive: 1.0, mix: 0.0, ..Default::default() }, &x, sr);
        let d = latency_samples(false);
        let err: f32 = (12000..20000).map(|i| (y[i + d] - x[i]).abs()).fold(0.0, f32::max);
        assert!(err < 0.02, "mix 0 differs by {err}");
        let z = run(Settings { style: TRIODE, drive: 1.0, punish: true, ..Default::default() }, &vec![0.0; 4800], sr);
        assert!(z.iter().all(|v| v.abs() < 1e-6), "silence in, noise out");
    }

    #[test]
    fn dc_is_blocked() {
        let sr = 48000.0;
        let x = sine(100.0, 0.7, sr, 96000);
        let y = run(Settings { style: TRIODE, drive: 1.0, ..Default::default() }, &x, sr);
        let s = &y[48000..96000];
        let mean = s.iter().sum::<f32>() / s.len() as f32;
        assert!(mean.abs() < 1e-3, "DC {mean}");
    }

    #[test]
    fn parameter_moves_do_not_click() {
        let sr = 48000.0;
        let x = sine(220.0, 0.3, sr, 48000);
        let mut c = Core::default();
        c.prepare(sr, 1);
        let mut set = Settings::default();
        c.set(&set);
        let mut y = x.clone();
        for (k, ch) in y.chunks_mut(128).enumerate() {
            if k == 100 {
                set.style = PENTODE;
                set.drive = 0.9;
                set.hq = true;
                set.steep = true;
                set.highcut = 5000.0;
                c.set(&set);
            }
            c.process_mono(ch);
        }
        // the largest sample-to-sample step stays near what a 220 Hz tone at this level makes
        let step = y.windows(2).skip(2000).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(step < 0.1, "step {step}");
        assert!(y.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn old_params_migrate_to_the_same_drive() {
        let mut p: std::collections::BTreeMap<String, f64> =
            [("drive".to_string(), 9.0), ("mix".to_string(), 80.0), ("out".to_string(), -3.0)].into_iter().collect();
        assert!(migrate_params(&mut p));
        assert!((drive_db(p["drive"] as f32 / 100.0) - 9.0).abs() < 1e-3);
        assert_eq!(p["style"], 0.0);
        assert_eq!(p["mix"], 80.0);
        assert!(!migrate_params(&mut p));
    }
}
