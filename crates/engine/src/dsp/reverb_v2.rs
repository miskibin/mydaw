//! Vocal-first algorithmic reverb ("Reverb" built-in, v2).
//!
//! Signal path (all allocation in `prepare`, none in `process`):
//!
//! ```text
//! dry ─┬─ wet input HPF/LPF ─ pre-delay ─┬─ velvet-noise early reflections ────────────┐
//!      │                                  └─ 4 allpass diffusers ─ 16-line FDN ─ late ─┤
//!      │                                                                               ├─ width ─ duck ─ mix
//!      └── envelope (ducking side-chain) ──────────────────────────────────────────────┘
//! ```
//!
//! * Late field: 16-line feedback delay network (Jot & Chaigne 1991; Schlecht & Habets 2017)
//!   with an orthonormal 16×16 Hadamard feedback matrix (fast Walsh–Hadamard), mutually
//!   incommensurate line lengths, and slow per-line delay modulation (cubic Hermite reads)
//!   so the modes never settle into a metallic ring.
//! * Frequency-dependent decay: every line carries a first-order low shelf and high shelf
//!   whose gains give the requested RT60 at DC (Bass), mid (Decay) and Nyquist (Tone)
//!   for that line's own length (Jot's absorption-filter rule `g = 10^(-3·L/T60)`).
//! * Early field: sparse velvet-noise taps (Järveläinen & Karjalainen 2007; Välimäki et al.
//!   2017) with different patterns per channel for decorrelation; none for the plate.
//! * Macro controls stay usable at every position: the decay is capped at 6 s, the
//!   high-band RT60 is always below the mid one, the network size follows the decay
//!   (short decays use short lines, long decays longer ones) and the wet level is
//!   partly normalised for decay, so "Mix" means about the same at every setting.

use crate::processor::{ParamBlock, ProcessCtx, Processor};

use super::{Biquad, FilterType};

/// Parameter slots, in the order of `catalog::params(FxKind::Reverb)`.
pub mod idx {
    pub const SIZE: usize = 0;
    pub const PRE: usize = 1;
    pub const LO: usize = 2;
    pub const HI: usize = 3;
    pub const WIDTH: usize = 4;
    pub const MIX: usize = 5;
    pub const TONE: usize = 6;
    pub const SPACE: usize = 7;
    pub const DUCK: usize = 8;
    pub const EARLY: usize = 9;
    pub const BASS: usize = 10;
    pub const MOD: usize = 11;
}

const N: usize = 16;
const ER_TAPS: usize = 24;
const DIFF: usize = 4;
const MAX_PRE_MS: f32 = 150.0;
const MAX_ER_MS: f32 = 120.0;
const MAX_SCALE: f32 = 1.8;
const MIN_SCALE: f32 = 0.6;
const MAX_MOD_MS: f32 = 0.6;
/// Longest decay the reverb accepts (s); longer only gets washy on a vocal.
pub const MAX_DECAY: f32 = 6.0;

/// Character of the space: line lengths, early reflections, diffusion.
#[derive(Clone, Copy)]
struct Space {
    /// shortest / longest FDN line at scale 1 (ms)
    lines: (f32, f32),
    /// decay at which the network has scale 1 (s)
    t_ref: f32,
    /// early reflections: span (ms) and level
    er_ms: f32,
    er_gain: f32,
    /// input allpass coefficient
    diff: f32,
    late_gain: f32,
}

const SPACES: [Space; 4] = [
    // Plate: dense from the first millisecond, no discrete reflections
    Space { lines: (11.0, 47.0), t_ref: 1.5, er_ms: 0.0, er_gain: 0.0, diff: 0.72, late_gain: 1.0 },
    // Room: strong short reflections, small network
    Space { lines: (7.0, 31.0), t_ref: 0.6, er_ms: 45.0, er_gain: 0.7, diff: 0.55, late_gain: 0.9 },
    // Hall: wide reflections, long lines
    Space { lines: (23.0, 97.0), t_ref: 2.0, er_ms: 110.0, er_gain: 0.45, diff: 0.65, late_gain: 1.0 },
    // Ambience: mostly early energy, a short soft tail
    Space { lines: (5.0, 23.0), t_ref: 0.5, er_ms: 35.0, er_gain: 1.0, diff: 0.5, late_gain: 0.6 },
];

const DIFF_MS: [[f32; DIFF]; 2] = [[3.13, 4.71, 7.93, 11.27], [3.41, 5.29, 7.13, 10.69]];
/// Output sign patterns: rows 5 and 10 of the 16×16 Sylvester Hadamard matrix (orthogonal → uncorrelated L/R).
const OUT_L: u16 = 0b0101_1010_0101_1010;
const OUT_R: u16 = 0b0110_0110_1001_1001;
/// Input sign pattern (decorrelates the injection across lines).
const IN_SIGN: u16 = 0b1011_0010_1101_0110;

#[inline]
fn sign(mask: u16, k: usize) -> f32 {
    if mask >> k & 1 == 1 {
        -1.0
    } else {
        1.0
    }
}

#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-18 {
        0.0
    } else {
        x
    }
}

/// Cubic Hermite (Catmull–Rom) interpolation between x0 and x1.
#[inline]
fn hermite(f: f32, xm1: f32, x0: f32, x1: f32, x2: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * f + c2) * f + c1) * f + x0
}

/// Power-of-two ring buffer with fractional reads. Delay `d` (samples) ≥ 2.
struct Ring {
    buf: Vec<f32>,
    mask: usize,
    w: usize,
}
impl Ring {
    fn new(min_len: usize) -> Self {
        let n = (min_len + 4).next_power_of_two();
        Self { buf: vec![0.0; n], mask: n - 1, w: 0 }
    }
    fn empty() -> Self {
        Self { buf: vec![0.0; 4], mask: 3, w: 0 }
    }
    #[inline]
    fn at(&self, d: usize) -> f32 {
        self.buf[self.w.wrapping_sub(d) & self.mask]
    }
    #[inline]
    fn read_cubic(&self, d: f32) -> f32 {
        let di = d as usize;
        let f = d - di as f32;
        hermite(f, self.at(di - 1), self.at(di), self.at(di + 1), self.at(di + 2))
    }
    #[inline]
    fn read_lin(&self, d: f32) -> f32 {
        let di = d as usize;
        let f = d - di as f32;
        let a = self.at(di);
        a + (self.at(di + 1) - a) * f
    }
    #[inline]
    fn push(&mut self, x: f32) {
        self.buf[self.w] = x;
        self.w = (self.w + 1) & self.mask;
    }
    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// Schroeder allpass with an integer delay.
struct Allpass {
    ring: Ring,
    d: usize,
}

/// First-order low shelf × first-order high shelf × mid gain: one line's absorption.
#[derive(Clone, Copy, Default)]
struct Absorb {
    g: f32,
    // low shelf (DC gain gl/gm, Nyquist 1)
    lb0: f32,
    lb1: f32,
    la1: f32,
    lz: f32,
    // high shelf (DC 1, Nyquist gh/gm)
    hb0: f32,
    hb1: f32,
    ha1: f32,
    hz: f32,
}
impl Absorb {
    /// `wl`, `wh`: prewarped crossovers tan(π f / sr).
    fn design(&mut self, gl: f32, gm: f32, gh: f32, wl: f32, wh: f32) {
        self.g = gm;
        // H(s) = (s + G·wc)/(s + wc), bilinear
        let gls = gl / gm;
        let d = 1.0 + wl;
        self.lb0 = (1.0 + gls * wl) / d;
        self.lb1 = (gls * wl - 1.0) / d;
        self.la1 = (wl - 1.0) / d;
        // H(s) = (G·s + wc)/(s + wc)
        let ghs = gh / gm;
        let d = 1.0 + wh;
        self.hb0 = (ghs + wh) / d;
        self.hb1 = (wh - ghs) / d;
        self.ha1 = (wh - 1.0) / d;
    }
    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        // transposed direct form I, first order
        let y = self.lb0 * x + self.lz;
        self.lz = flush(self.lb1 * x - self.la1 * y);
        let z = self.hb0 * y + self.hz;
        self.hz = flush(self.hb1 * y - self.ha1 * z);
        z * self.g
    }
}

/// In-place orthonormal 16-point Walsh–Hadamard transform.
#[inline]
fn wht16(o: &mut [f32; N]) {
    let mut h = 1;
    while h < N {
        let mut j = 0;
        while j < N {
            for k in j..j + h {
                let (a, b) = (o[k], o[k + h]);
                o[k] = a + b;
                o[k + h] = a - b;
            }
            j += h * 2;
        }
        h *= 2;
    }
    for v in o.iter_mut() {
        *v *= 0.25;
    }
}

/// Resolved, derived settings for a parameter set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Derived {
    pub t_mid: f32,
    pub t_low: f32,
    pub t_high: f32,
    pub x_low: f32,
    pub x_high: f32,
    pub space: usize,
    pub scale: f32,
}

/// Maps the user parameters to the decay targets per band (also used by tests and the tail estimate).
pub fn derive(v: &[f32; 12]) -> Derived {
    let space = (v[idx::SPACE].round().max(0.0) as usize).min(SPACES.len() - 1);
    let sp = SPACES[space];
    let t_mid = v[idx::SIZE].clamp(0.2, MAX_DECAY);
    let tone = (v[idx::TONE] / 100.0).clamp(-1.0, 1.0);
    // dark ↔ bright: highs die at 0.25 … 0.5 … 0.85 of the mid decay, crossover 1.5 … 3 … 6 kHz
    let hf_ratio = if tone < 0.0 { 0.5 + 0.25 * tone } else { 0.5 + 0.35 * tone };
    let bass = v[idx::BASS].clamp(0.5, 1.5);
    Derived {
        t_mid,
        t_low: t_mid * bass,
        t_high: t_mid * hf_ratio,
        x_low: 250.0,
        x_high: 3000.0 * 2f32.powf(tone),
        space,
        scale: (t_mid / sp.t_ref).powf(0.4).clamp(MIN_SCALE, MAX_SCALE),
    }
}

/// How long the effect keeps sounding after its input goes silent (s), down to about −90 dB.
/// Silence-skipping in the graph may stop processing this effect once that long has passed.
pub fn tail_seconds(v: &[f32; 12]) -> f32 {
    let d = derive(v);
    let sp = SPACES[d.space];
    v[idx::PRE].clamp(0.0, MAX_PRE_MS) / 1000.0 + sp.er_ms / 1000.0 + 1.5 * d.t_mid.max(d.t_low) + 0.05
}

/// The RT60 the network is designed to have at frequency `f` (s): the mean over lines of
/// −3·L / log10|H_line(f)|. Used by the tests (measured vs designed) and usable by a UI curve.
pub fn design_rt60(v: &[f32; 12], f: f32, sr: f32) -> f32 {
    let d = derive(v);
    let sp = SPACES[d.space];
    let wl = (std::f32::consts::PI * d.x_low / sr).tan();
    let wh = (std::f32::consts::PI * d.x_high.min(sr * 0.4) / sr).tan();
    let w = 2.0 * std::f32::consts::PI * f / sr;
    let (c, s) = (w.cos(), -w.sin()); // z^-1
    let mag1 = |b0: f32, b1: f32, a1: f32| {
        let (nr, ni) = (b0 + b1 * c, b1 * s);
        let (dr, di) = (1.0 + a1 * c, a1 * s);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    };
    let mut acc = 0f32;
    for k in 0..N {
        let len = ReverbV2::line_ms(&sp, k) * d.scale / 1000.0;
        let g = |t: f32| 10f32.powf(-3.0 * len / t);
        let mut a = Absorb::default();
        a.design(g(d.t_low), g(d.t_mid), g(d.t_high), wl, wh);
        let m = a.g * mag1(a.lb0, a.lb1, a.la1) * mag1(a.hb0, a.hb1, a.ha1);
        acc += -3.0 * len / m.log10();
    }
    acc / N as f32
}

/// Wet-level normalisation: long decays get louder, but only gently.
fn wet_norm(t_mid: f32) -> f32 {
    0.62 * t_mid.max(0.2).powf(-0.3)
}

pub struct ReverbV2 {
    sr: f32,
    v: [f32; 12],
    have_params: bool,
    // input
    hp: [Biquad; 2],
    lp: [Biquad; 2],
    pre: [Ring; 2],
    pre_target: f32,
    pre_cur: f32,
    // early reflections
    er_pos: [[f32; ER_TAPS]; 2],
    er_gain: [[f32; ER_TAPS]; 2],
    er_lvl: f32,
    er_lp: [f32; 2],
    er_lp_k: f32,
    // diffusers and FDN
    diff: [[Allpass; DIFF]; 2],
    diff_g: f32,
    lines: [Ring; N],
    len_target: [f32; N],
    len_cur: [f32; N],
    absorb: [Absorb; N],
    lfo: [(f32, f32); N],
    lfo_rot: [(f32, f32); N],
    mod_depth: f32,
    late_gain: f32,
    // output
    width: f32,
    width_cur: f32,
    wet_g: f32,
    dry_g: f32,
    wet_cur: f32,
    dry_cur: f32,
    norm: f32,
    duck_db: f32,
    env: f32,
    duck_cur: f32,
    k_param: f32,
    k_len: f32,
    k_env_att: f32,
    k_env_rel: f32,
    k_duck_att: f32,
    k_duck_rel: f32,
    wet: Vec<f32>,
    wet_n: usize,
}

impl Default for ReverbV2 {
    fn default() -> Self {
        let mut v = [0f32; 12];
        v[idx::SIZE] = 1.0;
        v[idx::PRE] = 25.0;
        v[idx::LO] = 300.0;
        v[idx::HI] = 7000.0;
        v[idx::WIDTH] = 100.0;
        v[idx::MIX] = 15.0;
        v[idx::EARLY] = 50.0;
        v[idx::BASS] = 1.0;
        v[idx::MOD] = 50.0;
        Self {
            sr: 0.0,
            v,
            have_params: false,
            hp: [Biquad::default(); 2],
            lp: [Biquad::default(); 2],
            pre: [Ring::empty(), Ring::empty()],
            pre_target: 0.0,
            pre_cur: 0.0,
            er_pos: [[2.0; ER_TAPS]; 2],
            er_gain: [[0.0; ER_TAPS]; 2],
            er_lvl: 0.0,
            er_lp: [0.0; 2],
            er_lp_k: 1.0,
            diff: std::array::from_fn(|_| std::array::from_fn(|_| Allpass { ring: Ring::empty(), d: 1 })),
            diff_g: 0.6,
            lines: std::array::from_fn(|_| Ring::empty()),
            len_target: [2.0; N],
            len_cur: [2.0; N],
            absorb: [Absorb::default(); N],
            lfo: [(1.0, 0.0); N],
            lfo_rot: [(1.0, 0.0); N],
            mod_depth: 0.0,
            late_gain: 1.0,
            width: 1.0,
            width_cur: 1.0,
            wet_g: 0.3,
            dry_g: 1.0,
            wet_cur: 0.3,
            dry_cur: 1.0,
            norm: 1.0,
            duck_db: 0.0,
            env: 0.0,
            duck_cur: 1.0,
            k_param: 1.0,
            k_len: 1.0,
            k_env_att: 1.0,
            k_env_rel: 1.0,
            k_duck_att: 1.0,
            k_duck_rel: 1.0,
            wet: vec![],
            wet_n: 0,
        }
    }
}

/// Small deterministic PRNG for the tap patterns (xorshift32).
fn rng(state: &mut u32) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

fn coef(sr: f32, ms: f32) -> f32 {
    1.0 - (-1.0 / (sr * ms / 1000.0)).exp()
}

impl ReverbV2 {
    fn line_ms(sp: &Space, k: usize) -> f32 {
        // geometric spread plus an irrational jitter keeps the lengths incommensurate
        let (a, b) = sp.lines;
        let t = k as f32 / (N - 1) as f32;
        let jitter = 1.0 + 0.06 * ((k as f32 * 0.618_034).fract() - 0.5);
        a * (b / a).powf(t) * jitter
    }

    fn apply(&mut self, first: bool) {
        if self.sr <= 0.0 {
            return;
        }
        let sr = self.sr;
        let v = self.v;
        let d = derive(&v);
        let sp = SPACES[d.space];
        // input filters on the wet
        let lo = v[idx::LO].clamp(20.0, 1000.0);
        let hi = v[idx::HI].clamp(1000.0, 20000.0).min(sr * 0.45);
        for c in 0..2 {
            self.hp[c].set(FilterType::Highpass, sr, lo, 0.707, 0.0);
            self.lp[c].set(FilterType::Lowpass, sr, hi, 0.707, 0.0);
        }
        self.pre_target = (v[idx::PRE].clamp(0.0, MAX_PRE_MS) / 1000.0 * sr).max(2.0);
        // early reflections: one velvet pulse per segment, sign random, level falls ~20 dB over the span
        let er_len = sp.er_ms / 1000.0 * sr;
        for c in 0..2 {
            let mut s = 0x9E37_79B9u32 ^ (c as u32 + 1).wrapping_mul(0x85EB_CA6B) ^ (d.space as u32) << 7;
            let mut sum = 0f32;
            for t in 0..ER_TAPS {
                let pos = (t as f32 + rng(&mut s)) / ER_TAPS as f32;
                let sg = if rng(&mut s) < 0.5 { -1.0 } else { 1.0 };
                self.er_pos[c][t] = 2.0 + pos * er_len;
                let g = (-2.3 * pos).exp();
                self.er_gain[c][t] = sg * g;
                sum += g * g;
            }
            let n = if sum > 0.0 { 1.0 / sum.sqrt() } else { 0.0 };
            for g in self.er_gain[c].iter_mut() {
                *g *= n;
            }
        }
        self.er_lvl = sp.er_gain * (v[idx::EARLY].clamp(0.0, 100.0) / 50.0);
        self.er_lp_k = 1.0 - (-2.0 * std::f32::consts::PI * (0.6 * hi).min(sr * 0.45) / sr).exp();
        self.diff_g = sp.diff;
        self.late_gain = sp.late_gain;
        // FDN lengths and absorption
        let wl = (std::f32::consts::PI * d.x_low / sr).tan();
        let wh = (std::f32::consts::PI * d.x_high.min(sr * 0.4) / sr).tan();
        for k in 0..N {
            let len = Self::line_ms(&sp, k) * d.scale / 1000.0 * sr;
            self.len_target[k] = len;
            if first {
                self.len_cur[k] = len;
            }
            let ls = len / sr;
            let g = |t: f32| 10f32.powf(-3.0 * ls / t);
            self.absorb[k].design(g(d.t_low), g(d.t_mid), g(d.t_high), wl, wh);
            // LFO: 0.25 … 1.1 Hz, spread across lines
            let hz = 0.25 + 0.85 * ((k as f32 * 0.381_966 + 0.13).fract());
            let w = 2.0 * std::f32::consts::PI * hz / sr;
            self.lfo_rot[k] = (w.cos(), w.sin());
        }
        if first {
            self.pre_cur = self.pre_target;
        }
        self.mod_depth = v[idx::MOD].clamp(0.0, 100.0) / 100.0 * MAX_MOD_MS / 1000.0 * sr;
        self.width = v[idx::WIDTH].clamp(0.0, 100.0) / 100.0;
        let m = v[idx::MIX].clamp(0.0, 100.0) / 100.0;
        self.wet_g = (2.0 * m).min(1.0);
        self.dry_g = (2.0 * (1.0 - m)).min(1.0);
        self.norm = wet_norm(d.t_mid);
        self.duck_db = v[idx::DUCK].clamp(0.0, 24.0);
        if first {
            self.wet_cur = self.wet_g;
            self.dry_cur = self.dry_g;
            self.width_cur = self.width;
        }
    }
}

impl Processor for ReverbV2 {
    fn prepare(&mut self, sr: f32, max: usize) {
        self.sr = sr;
        let ms = |x: f32| (x / 1000.0 * sr) as usize;
        self.pre = [Ring::new(ms(MAX_PRE_MS + MAX_ER_MS) + 8), Ring::new(ms(MAX_PRE_MS + MAX_ER_MS) + 8)];
        for c in 0..2 {
            for (i, a) in self.diff[c].iter_mut().enumerate() {
                let d = ms(DIFF_MS[c][i]).max(1);
                *a = Allpass { ring: Ring::new(d + 2), d };
            }
        }
        let longest = SPACES.iter().map(|s| s.lines.1).fold(0.0, f32::max) * 1.04 * MAX_SCALE + MAX_MOD_MS + 1.0;
        for ln in self.lines.iter_mut() {
            *ln = Ring::new(ms(longest) + 8);
        }
        for k in 0..N {
            let ph = k as f32 * 2.399_963; // golden angle: spread the LFO phases
            self.lfo[k] = (ph.cos(), ph.sin());
        }
        self.k_param = coef(sr, 30.0);
        self.k_len = coef(sr, 120.0);
        self.k_env_att = coef(sr, 5.0);
        self.k_env_rel = coef(sr, 200.0);
        self.k_duck_att = coef(sr, 15.0);
        self.k_duck_rel = coef(sr, 250.0);
        self.wet = vec![0.0; max.max(1)];
        self.reset();
        self.apply(true);
    }

    fn set_params(&mut self, p: &ParamBlock) {
        self.v = p.values;
        let first = !self.have_params;
        self.have_params = true;
        self.apply(first);
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        if self.sr <= 0.0 {
            return;
        }
        self.wet_n = l.len().min(self.wet.len());
        let kp = self.k_param;
        let kl = self.k_len;
        let dg = self.diff_g;
        let duck_on = self.duck_db > 0.0;
        // renormalise the LFO phasors once per block (rotation drifts by rounding)
        for p in self.lfo.iter_mut() {
            let m = 1.0 / (p.0 * p.0 + p.1 * p.1).sqrt();
            p.0 *= m;
            p.1 *= m;
        }
        for i in 0..l.len() {
            let (xl, xr) = (l[i], r[i]);
            // ducking side-chain from the dry input
            let pk = xl.abs().max(xr.abs());
            let ka = if pk > self.env { self.k_env_att } else { self.k_env_rel };
            self.env += (pk - self.env) * ka;
            let duck_t = if duck_on {
                let lvl = 20.0 * (self.env + 1e-9).log10();
                let amt = ((lvl + 45.0) / 30.0).clamp(0.0, 1.0);
                10f32.powf(-self.duck_db * amt / 20.0)
            } else {
                1.0
            };
            let kd = if duck_t < self.duck_cur { self.k_duck_att } else { self.k_duck_rel };
            self.duck_cur += (duck_t - self.duck_cur) * kd;

            // wet input: band-limit, pre-delay
            let inp = [self.lp[0].tick(self.hp[0].tick(xl)), self.lp[1].tick(self.hp[1].tick(xr))];
            self.pre[0].push(flush(inp[0]));
            self.pre[1].push(flush(inp[1]));
            self.pre_cur += (self.pre_target - self.pre_cur) * kp;
            let pd = self.pre_cur;
            let mut er = [0f32; 2];
            let mut dif = [0f32; 2];
            for c in 0..2 {
                let ring = &self.pre[c];
                if self.er_lvl > 0.0 {
                    let mut s = 0f32;
                    for t in 0..ER_TAPS {
                        s += ring.read_lin(pd + self.er_pos[c][t]) * self.er_gain[c][t];
                    }
                    self.er_lp[c] = flush(self.er_lp[c] + (s - self.er_lp[c]) * self.er_lp_k);
                    er[c] = self.er_lp[c] * self.er_lvl;
                }
                // input diffusion
                let mut x = ring.read_lin(pd);
                for a in self.diff[c].iter_mut() {
                    let z = a.ring.at(a.d);
                    let y = -dg * x + z;
                    a.ring.push(flush(x + dg * y));
                    x = y;
                }
                dif[c] = x;
            }

            // late field
            let mut o = [0f32; N];
            let mut late = [0f32; 2];
            for k in 0..N {
                self.len_cur[k] += (self.len_target[k] - self.len_cur[k]) * kl;
                let (c, s) = self.lfo[k];
                let (rc, rs) = self.lfo_rot[k];
                self.lfo[k] = (c * rc - s * rs, c * rs + s * rc);
                let d = self.len_cur[k] + self.mod_depth * (1.0 + s);
                let y = self.absorb[k].tick(self.lines[k].read_cubic(d.max(2.0)));
                o[k] = y;
                late[0] += sign(OUT_L, k) * y;
                late[1] += sign(OUT_R, k) * y;
            }
            wht16(&mut o);
            for k in 0..N {
                let x = dif[k & 1] * sign(IN_SIGN, k) * 0.35;
                self.lines[k].push(flush(o[k] + x));
            }

            // output
            let g = self.norm * self.duck_cur;
            let wl = (late[0] * 0.25 * self.late_gain + er[0]) * g;
            let wr = (late[1] * 0.25 * self.late_gain + er[1]) * g;
            self.width_cur += (self.width - self.width_cur) * kp;
            self.wet_cur += (self.wet_g - self.wet_cur) * kp;
            self.dry_cur += (self.dry_g - self.dry_cur) * kp;
            let mid = (wl + wr) * 0.5;
            let side = (wl - wr) * 0.5 * self.width_cur;
            if i < self.wet_n {
                self.wet[i] = mid;
            }
            l[i] = xl * self.dry_cur + (mid + side) * self.wet_cur;
            r[i] = xr * self.dry_cur + (mid - side) * self.wet_cur;
        }
    }

    fn reset(&mut self) {
        for c in 0..2 {
            self.pre[c].clear();
            self.hp[c].reset();
            self.lp[c].reset();
            for a in self.diff[c].iter_mut() {
                a.ring.clear();
            }
        }
        for ln in self.lines.iter_mut() {
            ln.clear();
        }
        for a in self.absorb.iter_mut() {
            a.lz = 0.0;
            a.hz = 0.0;
        }
        self.er_lp = [0.0; 2];
        self.env = 0.0;
        self.duck_cur = 1.0;
    }

    fn wet_tap(&self) -> Option<&[f32]> {
        Some(&self.wet[..self.wet_n])
    }
}
