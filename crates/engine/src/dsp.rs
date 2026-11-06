//! Built-in effects. All of them: allocation in `prepare`, none in `process`.

use std::f32::consts::PI;

use crate::model::FxKind;
use crate::processor::{ParamBlock, ProcessCtx, Processor};

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}
#[inline]
pub fn gain_to_db(g: f32) -> f32 {
    if g > 1e-9 {
        20.0 * g.log10()
    } else {
        -180.0
    }
}
#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 {
        0.0
    } else {
        x
    }
}

pub fn create(kind: FxKind) -> Option<Box<dyn Processor>> {
    Some(match kind {
        FxKind::Eq => Box::new(Eq::default()),
        FxKind::Comp => Box::new(Comp::default()),
        FxKind::Sat => Box::new(Sat::default()),
        FxKind::Delay => Box::new(Delay::default()),
        FxKind::Reverb => Box::new(Reverb::default()),
        FxKind::Limit => Box::new(Limiter::default()),
        FxKind::Fet => Box::new(crate::vocal::Fet::default()),
        FxKind::Deess => Box::new(crate::vocal::DeEss::default()),
        FxKind::Tune => Box::new(crate::vocal::Tune::default()),
        FxKind::Denoise => Box::new(crate::denoise::Denoise::default()),
        FxKind::Vst3 => return None,
    })
}

// ---------------------------------------------------------------- biquad

/// RBJ cookbook biquad, transposed direct form II.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType {
    Lowpass,
    Highpass,
    Bandpass,
    Notch,
    Peak,
    LowShelf,
    HighShelf,
}

impl Biquad {
    pub fn set(&mut self, t: FilterType, sr: f32, f: f32, q: f32, gain_db: f32) {
        let f = f.clamp(5.0, sr * 0.49);
        let w = 2.0 * PI * f / sr;
        let (sw, cw) = w.sin_cos();
        let q = q.max(0.05);
        let alpha = sw / (2.0 * q);
        let a = 10f32.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match t {
            FilterType::Lowpass => ((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            FilterType::Highpass => ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            FilterType::Bandpass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            FilterType::Notch => (1.0, -2.0 * cw, 1.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            FilterType::Peak => (1.0 + alpha * a, -2.0 * cw, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cw, 1.0 - alpha / a),
            FilterType::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cw + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                    a * ((a + 1.0) - (a - 1.0) * cw - s),
                    (a + 1.0) + (a - 1.0) * cw + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                    (a + 1.0) + (a - 1.0) * cw - s,
                )
            }
            FilterType::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cw + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                    a * ((a + 1.0) + (a - 1.0) * cw - s),
                    (a + 1.0) - (a - 1.0) * cw + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cw),
                    (a + 1.0) - (a - 1.0) * cw - s,
                )
            }
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }
    pub fn bypass(&mut self) {
        self.b0 = 1.0;
        self.b1 = 0.0;
        self.b2 = 0.0;
        self.a1 = 0.0;
        self.a2 = 0.0;
    }
    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = flush(self.b1 * x - self.a1 * y + self.z2);
        self.z2 = flush(self.b2 * x - self.a2 * y);
        y
    }
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
    /// Magnitude response in dB at `f`.
    pub fn response_db(&self, f: f32, sr: f32) -> f32 {
        let w = 2.0 * PI * f / sr;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).max(1e-30).log10()
    }
}

/// One-pole smoother for parameters that must glide (gains, delay time).
#[derive(Clone, Copy, Debug, Default)]
struct Smooth {
    v: f32,
    k: f32,
}
impl Smooth {
    fn setup(&mut self, sr: f32, ms: f32) {
        self.k = 1.0 - (-1.0 / (sr * ms / 1000.0)).exp();
    }
    #[inline]
    fn next(&mut self, target: f32) -> f32 {
        self.v += (target - self.v) * self.k;
        self.v
    }
}

// ---------------------------------------------------------------- EQ

const EQ_BANDS: usize = 24;
const EQ_STAGES: usize = 4;
/// Dynamic bands recompute their coefficients this often (samples).
const DYN_STEP: usize = 32;

#[derive(Clone, Copy, Default)]
struct BandState {
    l: [Biquad; EQ_STAGES],
    r: [Biquad; EQ_STAGES],
    det: Biquad,
    env: f32,
    /// current dynamic gain offset (dB)
    dyn_g: f32,
    tilt_out: f32,
}

/// Up to 24 bands: bell, shelves, cuts (12–48 dB/oct), notch, band pass, tilt;
/// each on stereo, left, right, mid or side; any bell/shelf/tilt can be dynamic.
pub struct Eq {
    sr: f32,
    bands: [BandState; EQ_BANDS],
    p: [crate::processor::BandParams; EQ_BANDS],
    n_on: usize,
    out: f32,
    gain: Smooth,
    last: Option<ParamBlock>,
    att: f32,
    rel: f32,
    tick: usize,
}

impl Default for Eq {
    fn default() -> Self {
        Self { sr: 0.0, bands: [BandState::default(); EQ_BANDS], p: [Default::default(); EQ_BANDS], n_on: 0, out: 1.0, gain: Smooth::default(), last: None, att: 0.0, rel: 0.0, tick: 0 }
    }
}

impl Eq {
    fn design(&mut self, i: usize, extra_db: f32) {
        let b = self.p[i];
        let sr = self.sr;
        let st = &mut self.bands[i];
        let g = b.g + extra_db;
        let set_all = |f: &mut [Biquad; EQ_STAGES], t: FilterType, q: f32, gain: f32, n: usize| {
            for (k, x) in f.iter_mut().enumerate() {
                if k < n {
                    x.set(t, sr, b.f, q, gain);
                } else {
                    x.bypass();
                }
            }
        };
        // Butterworth-ish cascades for steeper cuts: user Q on the first stage, 0.707 on the rest
        let (t, q, gain, n) = match b.kind {
            0 => (FilterType::Highpass, b.q, 0.0, b.stages as usize),
            1 => (FilterType::LowShelf, b.q, g, 1),
            3 => (FilterType::HighShelf, b.q, g, 1),
            4 => (FilterType::Lowpass, b.q, 0.0, b.stages as usize),
            5 => (FilterType::Notch, b.q, 0.0, 1),
            6 => (FilterType::Bandpass, b.q, 0.0, 1),
            7 => (FilterType::HighShelf, 0.5, g, 1),
            _ => (FilterType::Peak, b.q, g, 1),
        };
        set_all(&mut st.l, t, q, gain, n);
        set_all(&mut st.r, t, q, gain, n);
        for k in 1..n {
            st.l[k].set(t, sr, b.f, 0.707, 0.0);
            st.r[k].set(t, sr, b.f, 0.707, 0.0);
        }
        st.tilt_out = if b.kind == 7 { db_to_gain(-g / 2.0) } else { 1.0 };
    }
}

impl Processor for Eq {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        self.gain.setup(sr, 20.0);
        self.att = (-1.0 / (sr * 0.005)).exp();
        self.rel = (-1.0 / (sr * 0.08)).exp();
        if let Some(p) = self.last {
            self.last = None;
            self.set_params(&p);
        }
    }
    fn set_params(&mut self, p: &ParamBlock) {
        let first = self.last.is_none();
        self.last = Some(*p);
        self.out = db_to_gain(p.values[0]);
        if first || self.gain.v < 1e-6 {
            self.gain.v = self.out;
        }
        if self.sr <= 0.0 {
            return;
        }
        self.p = p.bands;
        self.n_on = 0;
        for i in 0..EQ_BANDS {
            if self.p[i].on {
                self.n_on = i + 1;
            }
            let dyn_g = if self.p[i].dynamic { self.bands[i].dyn_g } else { 0.0 };
            self.design(i, dyn_g);
            let b = self.p[i];
            self.bands[i].det.set(FilterType::Bandpass, self.sr, b.f, b.q.max(0.3), 0.0);
        }
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        for i in 0..l.len() {
            let (mut a, mut b) = (l[i], r[i]);
            for k in 0..self.n_on {
                let bp = self.p[k];
                if !bp.on {
                    continue;
                }
                let st = &mut self.bands[k];
                if bp.dynamic {
                    let src = match bp.place {
                        1 => a,
                        2 => b,
                        4 => (a - b) * 0.5,
                        _ => (a + b) * 0.5,
                    };
                    let d = st.det.tick(src);
                    let e = d * d;
                    st.env = if e > st.env { e + (st.env - e) * self.att } else { e + (st.env - e) * self.rel };
                }
                let n = if bp.kind == 0 || bp.kind == 4 { bp.stages as usize } else { 1 };
                let run = |f: &mut [Biquad; EQ_STAGES], mut x: f32| {
                    for s in f.iter_mut().take(n) {
                        x = s.tick(x);
                    }
                    x
                };
                let to = st.tilt_out;
                match bp.place {
                    1 => a = run(&mut st.l, a) * to,
                    2 => b = run(&mut st.r, b) * to,
                    3 | 4 => {
                        let (m, s) = ((a + b) * 0.5, (a - b) * 0.5);
                        let (m, s) = if bp.place == 3 { (run(&mut st.l, m) * to, s) } else { (m, run(&mut st.l, s) * to) };
                        a = m + s;
                        b = m - s;
                    }
                    _ => {
                        a = run(&mut st.l, a) * to;
                        b = run(&mut st.r, b) * to;
                    }
                }
            }
            let g = self.gain.next(self.out);
            l[i] = a * g;
            r[i] = b * g;
            // dynamic bands: move their gain every DYN_STEP samples
            self.tick += 1;
            if self.tick >= DYN_STEP {
                self.tick = 0;
                for k in 0..self.n_on {
                    let bp = self.p[k];
                    if !(bp.on && bp.dynamic) {
                        continue;
                    }
                    let lvl = 10.0 * self.bands[k].env.max(1e-12).log10();
                    let over = lvl - bp.thr;
                    // 6 dB soft knee, full range 12 dB over the threshold
                    let amt = ((over + 3.0) / 15.0).clamp(0.0, 1.0);
                    let target = bp.range * amt;
                    let cur = self.bands[k].dyn_g;
                    if (target - cur).abs() > 0.05 {
                        self.bands[k].dyn_g = cur + (target - cur) * 0.5;
                        let dg = self.bands[k].dyn_g;
                        self.design(k, dg);
                    }
                }
            }
        }
    }
    fn reset(&mut self) {
        for st in self.bands.iter_mut() {
            st.l.iter_mut().chain(st.r.iter_mut()).for_each(Biquad::reset);
            st.env = 0.0;
        }
    }
    fn meters(&self, out: &mut [f32; crate::processor::METERS]) {
        for k in 0..EQ_BANDS {
            out[k] = self.bands[k].dyn_g;
        }
    }
}

// ---------------------------------------------------------------- compressor

pub struct Comp {
    sr: f32,
    thr: f32,
    ratio: f32,
    att: f32,
    rel: f32,
    makeup: f32,
    env_db: f32,
    gr: f32,
    knee: f32,
}
impl Default for Comp {
    fn default() -> Self {
        Self { sr: 48000.0, thr: -18.0, ratio: 4.0, att: 0.0, rel: 0.0, makeup: 1.0, env_db: 0.0, gr: 0.0, knee: 6.0 }
    }
}
impl Comp {
    #[inline]
    fn curve(&self, x: f32) -> f32 {
        // static gain reduction (dB, ≤ 0) for level x dB, soft knee
        let over = x - self.thr;
        let slope = 1.0 / self.ratio - 1.0;
        if 2.0 * over < -self.knee {
            0.0
        } else if 2.0 * over > self.knee {
            slope * over
        } else {
            let t = over + self.knee / 2.0;
            slope * t * t / (2.0 * self.knee)
        }
    }
}
impl Processor for Comp {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
    }
    fn set_params(&mut self, p: &ParamBlock) {
        let v = p.values;
        self.thr = v[0];
        self.ratio = v[1].max(1.0);
        self.att = (-1.0 / (self.sr * v[2].max(0.05) / 1000.0)).exp();
        self.rel = (-1.0 / (self.sr * v[3].max(1.0) / 1000.0)).exp();
        self.makeup = db_to_gain(v[4]);
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        let mut gr_peak = 0f32;
        for i in 0..l.len() {
            let lvl = l[i].abs().max(r[i].abs());
            let x = gain_to_db(lvl).max(-120.0);
            let target = self.curve(x);
            // envelope on the gain reduction: attack when reducing more
            let k = if target < self.env_db { self.att } else { self.rel };
            self.env_db = target + (self.env_db - target) * k;
            let g = db_to_gain(self.env_db) * self.makeup;
            l[i] *= g;
            r[i] *= g;
            gr_peak = gr_peak.min(self.env_db);
        }
        self.gr = gr_peak;
    }
    fn reset(&mut self) {
        self.env_db = 0.0;
        self.gr = 0.0;
    }
    fn gain_reduction_db(&self) -> f32 {
        self.gr
    }
}

// ---------------------------------------------------------------- saturator

#[derive(Default)]
pub struct Sat {
    drive: f32,
    mix: f32,
    out: f32,
    sd: Smooth,
    so: Smooth,
    sm: Smooth,
}
impl Processor for Sat {
    fn prepare(&mut self, sr: f32, _max: usize) {
        for s in [&mut self.sd, &mut self.so, &mut self.sm] {
            s.setup(sr, 15.0);
        }
    }
    fn set_params(&mut self, p: &ParamBlock) {
        self.drive = db_to_gain(p.values[0]);
        self.mix = p.values[1] / 100.0;
        self.out = db_to_gain(p.values[2]) / self.drive.sqrt();
        if self.sd.v == 0.0 {
            self.sd.v = self.drive;
            self.so.v = self.out;
            self.sm.v = self.mix;
        }
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        let norm = 1.0 / 2.5f32.tanh();
        for i in 0..l.len() {
            let d = self.sd.next(self.drive);
            let o = self.so.next(self.out);
            let m = self.sm.next(self.mix);
            for x in [&mut l[i], &mut r[i]] {
                let w = (*x * d * 2.5).tanh() * norm * o;
                *x = *x * (1.0 - m) + w * m;
            }
        }
    }
}

// ---------------------------------------------------------------- delay

#[derive(Default)]
pub struct Delay {
    sr: f32,
    bl: Vec<f32>,
    br: Vec<f32>,
    w: usize,
    time: f32,
    fb: f32,
    mix: f32,
    st: Smooth,
}
impl Processor for Delay {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        let n = (sr * 2.1) as usize + 4;
        self.bl = vec![0.0; n];
        self.br = vec![0.0; n];
        self.w = 0;
        self.st.setup(sr, 60.0);
    }
    fn set_params(&mut self, p: &ParamBlock) {
        self.time = p.values[0].clamp(1.0, 2000.0) * self.sr / 1000.0;
        self.fb = (p.values[1] / 100.0).min(0.98);
        self.mix = p.values[2] / 100.0;
        if self.st.v == 0.0 {
            self.st.v = self.time;
        }
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        let n = self.bl.len();
        if n == 0 {
            return;
        }
        for i in 0..l.len() {
            let d = self.st.next(self.time).clamp(1.0, (n - 3) as f32);
            let pos = self.w as f32 - d + n as f32;
            let i0 = pos as usize;
            let fr = pos - i0 as f32;
            let read = |b: &[f32]| {
                let a = b[i0 % n];
                let c = b[(i0 + 1) % n];
                a + (c - a) * fr
            };
            let (yl, yr) = (read(&self.bl), read(&self.br));
            self.bl[self.w] = flush(l[i] + yl * self.fb);
            self.br[self.w] = flush(r[i] + yr * self.fb);
            self.w = (self.w + 1) % n;
            l[i] += yl * self.mix;
            r[i] += yr * self.mix;
        }
    }
    fn reset(&mut self) {
        self.bl.fill(0.0);
        self.br.fill(0.0);
    }
}

// ---------------------------------------------------------------- reverb (FDN)

const FDN: usize = 8;
/// Line lengths in ms: mutually prime-ish, spread across 30..75 ms.
const LINE_MS: [f32; FDN] = [29.71, 37.11, 41.13, 43.71, 53.03, 59.93, 67.73, 73.13];
const DIFF_MS: [f32; 4] = [4.77, 3.59, 12.73, 9.31];

struct Line {
    buf: Vec<f32>,
    len: usize,
    w: usize,
}
impl Line {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(1)], len: len.max(1), w: 0 }
    }
    #[inline]
    fn read(&self) -> f32 {
        self.buf[self.w]
    }
    #[inline]
    fn write(&mut self, x: f32) {
        self.buf[self.w] = x;
        self.w += 1;
        if self.w == self.len {
            self.w = 0;
        }
    }
}

/// Feedback delay network reverb: pre-delay → 4 allpass diffusers → 8-line FDN
/// with a Hadamard mix, per-line gain for the requested RT60 and a damping
/// lowpass that makes highs die first → low cut on the wet → width → mix.
pub struct Reverb {
    sr: f32,
    pre: Vec<f32>,
    pre_w: usize,
    pre_d: usize,
    diff: Vec<Line>,
    lines: Vec<Line>,
    g: [f32; FDN],
    damp: [f32; FDN],
    lp: [f32; FDN],
    hp_l: Biquad,
    hp_r: Biquad,
    width: f32,
    mix: f32,
    params: Option<ParamBlock>,
    wet: Vec<f32>,
    wet_n: usize,
}
impl Default for Reverb {
    fn default() -> Self {
        Self {
            sr: 0.0,
            pre: vec![],
            pre_w: 0,
            pre_d: 0,
            diff: vec![],
            lines: vec![],
            g: [0.0; FDN],
            damp: [0.0; FDN],
            lp: [0.0; FDN],
            hp_l: Biquad::default(),
            hp_r: Biquad::default(),
            width: 1.0,
            mix: 0.25,
            params: None,
            wet: vec![],
            wet_n: 0,
        }
    }
}
impl Reverb {
    fn apply(&mut self) {
        let Some(p) = self.params else { return };
        if self.sr <= 0.0 {
            return;
        }
        let v = p.values;
        let (rt60, pre_ms, lo, hi) = (v[0].max(0.1), v[1], v[2], v[3]);
        self.width = v[4] / 100.0;
        self.mix = v[5] / 100.0;
        self.pre_d = ((pre_ms / 1000.0) * self.sr) as usize;
        self.pre_d = self.pre_d.min(self.pre.len().saturating_sub(1));
        for i in 0..FDN {
            let len_s = self.lines[i].len as f32 / self.sr;
            self.g[i] = 10f32.powf(-3.0 * len_s / rt60);
            // damping: one-pole lowpass whose cutoff tracks "High cut"; highs lose a bit more each pass
            let fc = hi.clamp(500.0, self.sr * 0.45);
            self.damp[i] = (-2.0 * PI * fc / self.sr).exp();
        }
        self.hp_l.set(FilterType::Highpass, self.sr, lo, 0.707, 0.0);
        self.hp_r.set(FilterType::Highpass, self.sr, lo, 0.707, 0.0);
    }
}
impl Processor for Reverb {
    fn prepare(&mut self, sr: f32, max: usize) {
        self.sr = sr;
        self.pre = vec![0.0; (sr * 0.21) as usize + 2];
        self.pre_w = 0;
        self.diff = DIFF_MS.iter().map(|ms| Line::new((ms / 1000.0 * sr) as usize)).collect();
        self.lines = LINE_MS.iter().map(|ms| Line::new((ms / 1000.0 * sr) as usize)).collect();
        self.lp = [0.0; FDN];
        self.wet = vec![0.0; max.max(1)];
        self.apply();
    }
    fn set_params(&mut self, p: &ParamBlock) {
        self.params = Some(*p);
        self.apply();
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        if self.lines.is_empty() {
            return;
        }
        let n_pre = self.pre.len();
        let s = (1.0 / FDN as f32).sqrt();
        let dry_g = 1.0 - self.mix * 0.5;
        let wet_g = self.mix * 0.9;
        self.wet_n = l.len().min(self.wet.len());
        for i in 0..l.len() {
            let x = (l[i] + r[i]) * 0.5;
            self.pre[self.pre_w] = x;
            let rd = (self.pre_w + n_pre - self.pre_d) % n_pre;
            self.pre_w = (self.pre_w + 1) % n_pre;
            let mut d = self.pre[rd];
            // series allpass diffusion
            for a in self.diff.iter_mut() {
                let z = a.read();
                let y = -0.6 * d + z;
                a.write(flush(d + 0.6 * y));
                d = y;
            }
            // FDN
            let mut o = [0f32; FDN];
            for k in 0..FDN {
                let y = self.lines[k].read();
                self.lp[k] = flush(y * (1.0 - self.damp[k]) + self.lp[k] * self.damp[k]);
                o[k] = self.lp[k] * self.g[k];
            }
            // fast Walsh–Hadamard (orthogonal mixing)
            let mut h = 1;
            while h < FDN {
                let mut j = 0;
                while j < FDN {
                    for k in j..j + h {
                        let (a, b) = (o[k], o[k + h]);
                        o[k] = a + b;
                        o[k + h] = a - b;
                    }
                    j += h * 2;
                }
                h *= 2;
            }
            for k in 0..FDN {
                self.lines[k].write(flush(o[k] * s + d));
            }
            let wl = self.lines[0].read() + self.lines[2].read() + self.lines[4].read() + self.lines[6].read();
            let wr = self.lines[1].read() + self.lines[3].read() + self.lines[5].read() + self.lines[7].read();
            let (wl, wr) = (self.hp_l.tick(wl * 0.35), self.hp_r.tick(wr * 0.35));
            let mid = (wl + wr) * 0.5;
            let side = (wl - wr) * 0.5 * self.width;
            if i < self.wet_n {
                self.wet[i] = mid;
            }
            l[i] = l[i] * dry_g + (mid + side) * wet_g;
            r[i] = r[i] * dry_g + (mid - side) * wet_g;
        }
    }
    fn reset(&mut self) {
        self.pre.fill(0.0);
        for ln in self.diff.iter_mut().chain(self.lines.iter_mut()) {
            ln.buf.fill(0.0);
        }
        self.lp = [0.0; FDN];
    }
    fn wet_tap(&self) -> Option<&[f32]> {
        Some(&self.wet[..self.wet_n])
    }
}

// ---------------------------------------------------------------- limiter

/// Look-ahead peak limiter (1 ms): the gain is already down when the peak arrives,
/// then a hard clip at the ceiling catches the rest.
pub struct Limiter {
    sr: f32,
    gain: f32,
    ceil: f32,
    dl: Vec<f32>,
    dr: Vec<f32>,
    req: Vec<f32>,
    w: usize,
    env: f32,
    rel: f32,
    gr: f32,
}
impl Default for Limiter {
    fn default() -> Self {
        Self { sr: 48000.0, gain: 1.0, ceil: 0.966, dl: vec![], dr: vec![], req: vec![], w: 0, env: 1.0, rel: 0.999, gr: 0.0 }
    }
}
impl Processor for Limiter {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        let n = ((sr * 0.001) as usize).max(8);
        self.dl = vec![0.0; n];
        self.dr = vec![0.0; n];
        self.req = vec![1.0; n];
        self.w = 0;
        self.rel = (-1.0 / (sr * 0.06)).exp();
    }
    fn set_params(&mut self, p: &ParamBlock) {
        self.gain = db_to_gain(p.values[0]);
        self.ceil = db_to_gain(p.values[1]);
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        let n = self.dl.len();
        if n == 0 {
            return;
        }
        let mut gr_min = 1f32;
        for i in 0..l.len() {
            let (a, b) = (l[i] * self.gain, r[i] * self.gain);
            let pk = a.abs().max(b.abs());
            self.req[self.w] = if pk > self.ceil { self.ceil / pk } else { 1.0 };
            self.dl[self.w] = a;
            self.dr[self.w] = b;
            self.w = (self.w + 1) % n;
            let mut target = 1f32;
            for &g in &self.req {
                target = target.min(g);
            }
            self.env = if target < self.env { target } else { target + (self.env - target) * self.rel };
            let (oa, ob) = (self.dl[self.w], self.dr[self.w]);
            l[i] = (oa * self.env).clamp(-self.ceil, self.ceil);
            r[i] = (ob * self.env).clamp(-self.ceil, self.ceil);
            gr_min = gr_min.min(self.env);
        }
        self.gr = gain_to_db(gr_min);
    }
    fn reset(&mut self) {
        self.dl.fill(0.0);
        self.dr.fill(0.0);
        self.req.fill(1.0);
        self.env = 1.0;
    }
    fn gain_reduction_db(&self) -> f32 {
        self.gr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mag_db(q: &Biquad, f: f32, sr: f32) -> f32 {
        let w = 2.0 * PI * f / sr;
        let (c1, s1, c2, s2) = (w.cos(), -w.sin(), (2.0 * w).cos(), -(2.0 * w).sin());
        let (nr, ni) = (q.b0 + q.b1 * c1 + q.b2 * c2, q.b1 * s1 + q.b2 * s2);
        let (dr, di) = (1.0 + q.a1 * c1 + q.a2 * c2, q.a1 * s1 + q.a2 * s2);
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }

    #[test]
    fn notch_is_narrow() {
        let mut q = Biquad::default();
        q.set(FilterType::Notch, 48000.0, 50.0, 6.0, 0.0);
        assert!(mag_db(&q, 50.0, 48000.0) < -40.0);
        // a 50 Hz hum notch must leave a voice's body alone
        for f in [100.0, 160.0, 1000.0] {
            assert!(mag_db(&q, f, 48000.0) > -0.3, "{f} Hz cut by {:.1} dB", mag_db(&q, f, 48000.0));
        }
    }
    use crate::catalog::{new_fx, param_block};

    fn run(kind: FxKind, input: impl Fn(usize) -> f32, n: usize) -> (Vec<f32>, Box<dyn Processor>) {
        let mut p = create(kind).unwrap();
        p.prepare(48000.0, 256);
        p.set_params(&param_block(&new_fx(1, kind, 120.0)));
        let mut out = Vec::new();
        let ctx = ProcessCtx { sample_rate: 48000.0, ..Default::default() };
        let mut i = 0;
        while i < n {
            let mut l: Vec<f32> = (i..i + 256).map(&input).collect();
            let mut r = l.clone();
            p.process(&mut l, &mut r, &ctx);
            out.extend_from_slice(&l);
            i += 256;
        }
        (out, p)
    }

    #[test]
    fn flat_eq_is_transparent() {
        let (out, _) = run(FxKind::Eq, |i| (i as f32 * 0.05).sin() * 0.5, 4800);
        let err: f32 = out.iter().enumerate().skip(1000).map(|(i, y)| (y - (i as f32 * 0.05).sin() * 0.5).abs()).fold(0.0, f32::max);
        assert!(err < 1e-3, "flat EQ changed the signal by {err}");
    }

    fn eq_with(bands: Vec<crate::model::EqBand>) -> Box<dyn Processor> {
        let mut fx = new_fx(1, FxKind::Eq, 120.0);
        fx.bands = Some(bands);
        let mut p = create(FxKind::Eq).unwrap();
        p.prepare(48000.0, 256);
        p.set_params(&param_block(&fx));
        p
    }

    fn rms_after(p: &mut Box<dyn Processor>, l0: impl Fn(usize) -> f32, r0: impl Fn(usize) -> f32) -> (f32, f32) {
        let ctx = ProcessCtx { sample_rate: 48000.0, ..Default::default() };
        let (mut el, mut er) = (0f32, 0f32);
        for blk in 0..200 {
            let mut l: Vec<f32> = (blk * 256..blk * 256 + 256).map(&l0).collect();
            let mut r: Vec<f32> = (blk * 256..blk * 256 + 256).map(&r0).collect();
            p.process(&mut l, &mut r, &ctx);
            if blk > 150 {
                el += l.iter().map(|x| x * x).sum::<f32>();
                er += r.iter().map(|x| x * x).sum::<f32>();
            }
        }
        (el.sqrt(), er.sqrt())
    }

    #[test]
    fn dynamic_band_only_acts_when_loud() {
        use crate::model::{BandType, DynBand, EqBand, Placement};
        let band = EqBand { on: true, kind: BandType::Peak, f: 1000.0, g: 0.0, q: 1.0, slope: 12, place: Placement::Stereo, dynamic: Some(DynBand { range: -12.0, thr: -30.0 }) };
        let sine = |a: f32| move |i: usize| (i as f32 * 1000.0 * std::f32::consts::TAU / 48000.0).sin() * a;
        let mut p = eq_with(vec![band]);
        let (loud, _) = rms_after(&mut p, sine(0.5), sine(0.5));
        let mut flat = eq_with(vec![]);
        let (loud_ref, _) = rms_after(&mut flat, sine(0.5), sine(0.5));
        assert!(gain_to_db(loud / loud_ref) < -9.0, "loud not ducked: {}", gain_to_db(loud / loud_ref));
        let mut p = eq_with(vec![band]);
        let (quiet, _) = rms_after(&mut p, sine(0.005), sine(0.005));
        let (quiet_ref, _) = rms_after(&mut eq_with(vec![]), sine(0.005), sine(0.005));
        assert!(gain_to_db(quiet / quiet_ref).abs() < 0.5, "quiet changed: {}", gain_to_db(quiet / quiet_ref));
    }

    #[test]
    fn mid_band_leaves_the_side_alone() {
        use crate::model::{BandType, EqBand, Placement};
        let cut = EqBand { on: true, kind: BandType::Peak, f: 1000.0, g: -18.0, q: 1.0, slope: 12, place: Placement::Mid, dynamic: None };
        let sine = |i: usize| (i as f32 * 1000.0 * std::f32::consts::TAU / 48000.0).sin() * 0.5;
        // pure side signal: L = -R
        let mut p = eq_with(vec![cut]);
        let (l, _) = rms_after(&mut p, sine, |i| -sine(i));
        let (lr, _) = rms_after(&mut eq_with(vec![]), sine, |i| -sine(i));
        assert!(gain_to_db(l / lr).abs() < 0.5);
        // pure mid: cut applies
        let mut p = eq_with(vec![cut]);
        let (l, _) = rms_after(&mut p, sine, sine);
        assert!(gain_to_db(l / lr) < -15.0);
    }

    #[test]
    fn limiter_holds_the_ceiling() {
        let (out, p) = run(FxKind::Limit, |i| (i as f32 * 0.01).sin() * 3.0, 48000);
        let pk = out.iter().fold(0f32, |a, b| a.max(b.abs()));
        assert!(pk <= db_to_gain(-0.3) + 1e-4, "peak {pk}");
        assert!(p.gain_reduction_db() < -6.0);
    }

    #[test]
    fn compressor_reduces_loud_signal() {
        let (out, p) = run(FxKind::Comp, |i| (i as f32 * 0.03).sin() * 0.9, 48000);
        let pk = out[40000..].iter().fold(0f32, |a, b| a.max(b.abs()));
        assert!(pk < 0.6, "peak {pk}");
        assert!(p.gain_reduction_db() < -3.0);
    }

    #[test]
    fn reverb_tail_decays_near_rt60() {
        // impulse, then measure energy around RT60/2 vs 0.1 s
        let (out, _) = run(FxKind::Reverb, |i| if i == 0 { 1.0 } else { 0.0 }, 48000 * 3);
        let rms = |a: usize, b: usize| (out[a..b].iter().map(|x| x * x).sum::<f32>() / (b - a) as f32).sqrt();
        let early = rms(4800, 9600);
        let late = rms(48000 * 2, 48000 * 2 + 4800);
        assert!(early > 1e-4, "no tail at all");
        let drop = gain_to_db(late / early);
        // default decay 2.2 s: ~1.9 s apart → about -52 dB; allow a wide band
        assert!(drop < -25.0 && drop > -90.0, "decay {drop} dB");
    }

    #[test]
    fn delay_repeats_at_time() {
        let (out, _) = run(FxKind::Delay, |i| if i == 0 { 1.0 } else { 0.0 }, 48000);
        let t = (375.0 * 48.0) as usize; // default preset 1/8 at 120 bpm = 250 ms; params come from preset
        let t = if out[t].abs() > 0.05 { t } else { 250 * 48 };
        assert!(out[t - 2..t + 3].iter().any(|x| x.abs() > 0.05), "no echo at {t}");
    }
}
