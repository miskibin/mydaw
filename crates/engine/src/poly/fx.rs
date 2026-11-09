//! The synth's global effects after the voice sum: drive → chorus → ping-pong delay → reverb.
//! Buffers are allocated in [`Fx::new`] (off the audio thread); processing never allocates.
//! The reverb is the engine's own `reverb_v2`.

use std::f32::consts::TAU;

use super::params::{ix, Patch, DELAY_BEATS};
use super::tables::bank;
use super::voice::{tanh, Os2};
use crate::dsp::reverb_v2::{idx, ReverbV2};
use crate::processor::{ParamBlock, ProcessCtx, Processor};

#[inline(always)]
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 {
        0.0
    } else {
        x
    }
}

/// A delay line with linear-interpolated reads (power-of-two ring).
struct Line {
    buf: Vec<f32>,
    mask: usize,
    w: usize,
}

impl Line {
    fn new(min_len: usize) -> Self {
        let n = min_len.next_power_of_two();
        Self { buf: vec![0.0; n], mask: n - 1, w: 0 }
    }
    #[inline(always)]
    fn push(&mut self, x: f32) {
        self.buf[self.w] = x;
        self.w = (self.w + 1) & self.mask;
    }
    /// `d` samples behind the newest (d ≥ 1).
    #[inline(always)]
    fn read(&self, d: f32) -> f32 {
        let pos = self.w as f32 - d + self.buf.len() as f32;
        let i = pos as usize;
        let fr = pos - i as f32;
        let a = self.buf[i & self.mask];
        let b = self.buf[(i + 1) & self.mask];
        a + (b - a) * fr
    }
    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// Two-voice stereo chorus (quadrature modulation between the sides).
struct Chorus {
    l: Line,
    r: Line,
    ph1: f32,
    ph2: f32,
}

impl Chorus {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], mix: f32, sr: f32) {
        let ms = sr / 1000.0;
        let (d1, m1, d2, m2) = (11.0 * ms, 2.2 * ms, 17.0 * ms, 3.1 * ms);
        let (s1, s2) = (0.43 / sr, 0.27 / sr);
        for i in 0..l.len() {
            self.l.push(l[i]);
            self.r.push(r[i]);
            let (a, b) = ((TAU * self.ph1).sin(), (TAU * self.ph2).sin());
            let (ac, bc) = ((TAU * self.ph1).cos(), (TAU * self.ph2).cos());
            let wl = 0.5 * (self.l.read(d1 + m1 * a) + self.l.read(d2 + m2 * b));
            let wr = 0.5 * (self.r.read(d1 + m1 * ac) + self.r.read(d2 - m2 * bc));
            l[i] = l[i] * (1.0 - 0.35 * mix) + wl * mix;
            r[i] = r[i] * (1.0 - 0.35 * mix) + wr * mix;
            self.ph1 = (self.ph1 + s1).fract();
            self.ph2 = (self.ph2 + s2).fract();
        }
    }
}

/// Ping-pong delay: the mono input enters the left line, each line feeds the other, so the
/// repeats alternate sides. The feedback path is band-limited (dark, thin repeats).
struct PingPong {
    l: Line,
    r: Line,
    d: f32,
    lp: f32,
    hp: f32,
}

impl PingPong {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], mix: f32, fb: f32, target: f32, sr: f32) {
        let max = (self.l.buf.len() - 4) as f32;
        let target = target.clamp(1.0, max);
        let kd = 1.0 - (-1.0 / (0.06 * sr)).exp();
        let alp = 1.0 - (-TAU * 5500.0 / sr).exp();
        let ahp = 1.0 - (-TAU * 140.0 / sr).exp();
        for i in 0..l.len() {
            self.d += (target - self.d) * kd;
            let yl = self.l.read(self.d);
            let yr = self.r.read(self.d);
            let x = 0.5 * (l[i] + r[i]) + yr * fb;
            self.lp = flush(self.lp + (x - self.lp) * alp);
            self.hp = flush(self.hp + (self.lp - self.hp) * ahp);
            self.l.push(self.lp - self.hp);
            self.r.push(flush(yl));
            l[i] += yl * mix;
            r[i] += yr * mix;
        }
    }
}

pub struct Fx {
    sr: f32,
    os: [Os2; 2],
    chorus: Chorus,
    delay: PingPong,
    reverb: ReverbV2,
    rv_vals: [f32; 12],
}

impl Fx {
    /// Allocates for `sr` and chunks of at most `max_chunk` samples.
    pub fn new(sr: f32, max_chunk: usize) -> Self {
        let mut reverb = ReverbV2::default();
        reverb.prepare(sr, max_chunk);
        let delay_len = (sr * 2.6) as usize + 8;
        Self {
            sr,
            os: [Os2::default(); 2],
            chorus: Chorus { l: Line::new((sr * 0.03) as usize + 8), r: Line::new((sr * 0.03) as usize + 8), ph1: 0.0, ph2: 0.25 },
            delay: PingPong { l: Line::new(delay_len), r: Line::new(delay_len), d: 0.0, lp: 0.0, hp: 0.0 },
            reverb,
            rv_vals: [f32::NAN; 12],
        }
    }

    /// Longest time the effects can keep sounding after silence comes in, in samples.
    pub fn tail(&self, p: &Patch, bpm: f64) -> usize {
        let mut t = 0.05f32;
        if p[ix::FX_DELAY] > 0.0 {
            let d = Self::delay_secs(p, bpm);
            let fb = (p[ix::FX_DFB] * 0.01).max(0.01);
            t = t.max(d * (1.0 + (-6.9 / fb.ln()).min(40.0)));
        }
        if p[ix::FX_REVERB] > 0.0 {
            t = t.max(p[ix::FX_RSIZE] * 1.5);
        }
        (t * self.sr) as usize
    }

    fn delay_secs(p: &Patch, bpm: f64) -> f32 {
        let beats = DELAY_BEATS[(p[ix::FX_DTIME] as usize).min(DELAY_BEATS.len() - 1)];
        (beats * 60.0 / bpm.max(20.0) as f32).min(2.5)
    }

    pub fn reset(&mut self) {
        for o in self.os.iter_mut() {
            o.reset();
        }
        self.chorus.l.clear();
        self.chorus.r.clear();
        self.delay.l.clear();
        self.delay.r.clear();
        self.delay.lp = 0.0;
        self.delay.hp = 0.0;
        self.reverb.reset();
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], p: &Patch, bpm: f64) {
        let n = l.len();
        let sr = self.sr;
        // drive: always through the oversampler, so turning it up never shifts the timing
        let dr = p[ix::FX_DRIVE] * 0.01;
        let hb = &bank().hb;
        if dr > 0.0 {
            let g = 1.0 + 30.0 * dr * dr;
            let comp = g.powf(-0.4);
            // the first 5 % of the knob blend in the shaper, so leaving 0 is continuous
            let w = (dr * 20.0).min(1.0);
            let f = |x: f32| x + (tanh(g * x) * comp - x) * w;
            for i in 0..n {
                l[i] = self.os[0].process(l[i], hb, f);
                r[i] = self.os[1].process(r[i], hb, f);
            }
        } else {
            for i in 0..n {
                l[i] = self.os[0].process(l[i], hb, |x| x);
                r[i] = self.os[1].process(r[i], hb, |x| x);
            }
        }
        // Chorus, delay and reverb always run (while the instance is not idle) even at zero
        // mix: their lines then hold the recent signal, so turning a mix up (a preset change
        // on a held note) fades in a continuous wet signal instead of a wet signal that
        // starts with a jump.
        let ch = p[ix::FX_CHORUS] * 0.01;
        self.chorus.process(l, r, ch, sr);
        let dm = p[ix::FX_DELAY] * 0.01;
        let dsec = Self::delay_secs(p, bpm);
        if self.delay.d == 0.0 {
            self.delay.d = dsec * sr;
        }
        self.delay.process(l, r, dm, (p[ix::FX_DFB] * 0.01).min(0.9), dsec * sr, sr);
        let rm = p[ix::FX_REVERB];
        {
            let mut v = [0f32; 12];
            v[idx::SIZE] = p[ix::FX_RSIZE];
            v[idx::PRE] = 12.0;
            v[idx::LO] = 220.0;
            v[idx::HI] = 7500.0;
            v[idx::WIDTH] = 100.0;
            v[idx::MIX] = rm;
            v[idx::TONE] = -10.0;
            v[idx::SPACE] = 2.0;
            v[idx::DUCK] = 0.0;
            v[idx::EARLY] = 40.0;
            v[idx::BASS] = 0.85;
            v[idx::MOD] = 60.0;
            if v != self.rv_vals {
                self.rv_vals = v;
                self.reverb.set_params(&ParamBlock { values: v, ..Default::default() });
            }
            self.reverb.process(l, r, &ProcessCtx { sample_rate: sr, bpm, beat: 0.0, playing: false });
        }
    }
}
