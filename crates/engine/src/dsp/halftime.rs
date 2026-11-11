//! Half-Speed: a tempo-locked slow-down. Every loop window (1/16 note … 8 bars) the read head
//! restarts at the write head and then plays the window's start at 1/1.5, 1/2 or 1/4 speed, so
//! each hit sounds a fifth, an octave or two octaves lower while the groove stays on the grid.
//!
//! ```text
//!            ┌─ low ──(all-pass at f_hi)───────────────────────┐
//!  in ─ LR4 ─┤                                                 ├─ bypass ─┐
//!   at f_lo  └─ mid/high ─ LR4 at f_hi ─┬─ high ───────────────┘          │
//!                                       └─ mid = active band ─ ring ─ A/B heads ─ + ─ mix ─ out
//! ```
//!
//! * Phase comes from the host beat (`ProcessCtx::beat`), never from a free-running counter, so a
//!   jump of the playhead lands in the right place of the window at once. Content written before
//!   the jump is never read: until the next window starts the effect passes the input through.
//! * The read position is a fractional delay behind the write head, `d = local · (1 − 1/M)`,
//!   read with a 4-point Hermite interpolator from a power-of-two ring.
//! * Smooth > 0 (Sustain → Swell): at the window start a tail head B keeps playing the old window
//!   past its end while the new one fades in (equal power, 1 ms … 500 ms; long = auto-swell).
//! * Smooth < 0 (Percussive → FX): the crossfade happens *before* the window start, so the hit on
//!   the grid is at full gain. The incoming head reads ever more recent input, which at long
//!   settings is heard as a pitch-up swoop into each window.
//! * Speed or window changes while playing hand the old trajectory to head B for 10 ms.
//! * Stopped transport: the effect passes the input through (the split bands still sum flat).

use super::{Biquad, FilterType};
use crate::processor::{ParamBlock, ProcessCtx, Processor, METERS};

/// Order of the parameter list in `catalog::params(FxKind::Half)`.
pub mod idx {
    pub const SPEED: usize = 0;
    pub const LOOP: usize = 1;
    pub const SMOOTH: usize = 2;
    pub const LO: usize = 3;
    pub const HI: usize = 4;
    pub const SOLO: usize = 5;
    pub const MIX: usize = 6;
}

pub const SPEEDS: [&str; 3] = ["1.5×", "2×", "4×"];
pub const SPEED_DIV: [f64; 3] = [1.5, 2.0, 4.0];
pub const LOOPS: [&str; 8] = ["1/16", "1/8", "1/4", "1/2", "1 takt", "2 takty", "4 takty", "8 taktów"];
/// Window lengths in beats (4/4).
pub const LOOP_BEATS: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];

/// Band edges at or beyond these leave that side unsplit.
const LO_OFF: f32 = 21.0;
const HI_OFF: f32 = 19_500.0;
const Q_BW: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Smooth knob (−100 … 100 %) → crossfade length in ms. Positive: after the window start
/// (Sustain/Swell), negative: before it (Percussive/FX).
pub fn smooth_ms(v: f32) -> f32 {
    let t = (v.abs() / 100.0).clamp(0.0, 1.0);
    if v >= 0.0 {
        1.0 + 499.0 * t.powf(2.2)
    } else {
        1.0 + 299.0 * t * t
    }
}

/// Linkwitz-Riley 4th order: two identical Butterworth biquads.
#[derive(Default, Clone, Copy)]
struct Lr4 {
    a: Biquad,
    b: Biquad,
}

impl Lr4 {
    fn set(&mut self, t: FilterType, sr: f32, f: f32) {
        self.a.set(t, sr, f, Q_BW, 0.0);
        self.b.set(t, sr, f, Q_BW, 0.0);
    }
    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        self.b.tick(self.a.tick(x))
    }
    fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
    }
}

/// One channel's three-band split.
#[derive(Default, Clone, Copy)]
struct Split {
    lp_lo: Lr4,
    hp_lo: Lr4,
    lp_hi: Lr4,
    hp_hi: Lr4,
    // the low band through an LR4 pair at f_hi (sum = all-pass) to match the mid/high phase
    ap_lp: Lr4,
    ap_hp: Lr4,
}

impl Split {
    fn set(&mut self, sr: f32, lo: f32, hi: f32) {
        self.lp_lo.set(FilterType::Lowpass, sr, lo);
        self.hp_lo.set(FilterType::Highpass, sr, lo);
        self.lp_hi.set(FilterType::Lowpass, sr, hi);
        self.hp_hi.set(FilterType::Highpass, sr, hi);
        self.ap_lp.set(FilterType::Lowpass, sr, hi);
        self.ap_hp.set(FilterType::Highpass, sr, hi);
    }
    /// (active, bypassed) for the given split layout.
    #[inline]
    fn tick(&mut self, x: f32, lo_on: bool, hi_on: bool) -> (f32, f32) {
        match (lo_on, hi_on) {
            (false, false) => (x, 0.0),
            (true, false) => (self.hp_lo.tick(x), self.lp_lo.tick(x)),
            (false, true) => (self.lp_hi.tick(x), self.hp_hi.tick(x)),
            (true, true) => {
                let low = self.lp_lo.tick(x);
                let mh = self.hp_lo.tick(x);
                let mid = self.lp_hi.tick(mh);
                let high = self.hp_hi.tick(mh);
                let low = self.ap_lp.tick(low) + self.ap_hp.tick(low);
                (mid, low + high)
            }
        }
    }
    fn reset(&mut self) {
        for f in [&mut self.lp_lo, &mut self.hp_lo, &mut self.lp_hi, &mut self.hp_hi, &mut self.ap_lp, &mut self.ap_hp] {
            f.reset();
        }
    }
}

#[derive(Default)]
pub struct HalfTime {
    sr: f32,
    bl: Vec<f32>,
    br: Vec<f32>,
    mask: usize,
    w: usize,
    /// Samples written since prepare.
    t: u64,
    /// First sample of continuous playback: nothing older is read.
    valid_from: u64,
    /// Beat the next block should start at (jump detection).
    expect: Option<f64>,
    prev_local: f64,

    div: f64,
    loop_beats: f64,
    smooth: f32,
    lo: f32,
    hi: f32,
    solo: bool,
    mix: f32,
    mix_s: f32,
    split: [Split; 2],
    lo_on: bool,
    hi_on: bool,

    /// Head B: delay, how fast it grows per sample, samples left of its fade, fade length.
    tail_d: f64,
    tail_slope: f64,
    tail_left: u32,
    tail_len: u32,
    /// Head A as it was one sample ago (handover on parameter changes).
    last_d: f64,
    last_slope: f64,
    last_valid: bool,

    m_phase: f32,
    m_active: f32,
    m_ga: f32,
    m_gb: f32,
    m_max_d: f32,
}

impl HalfTime {
    #[inline]
    fn read(buf: &[f32], mask: usize, w: usize, d: f64) -> f32 {
        // newest sample is at w - 1; d = 0 reads it
        let pos = w as f64 - 1.0 - d;
        let k = pos.floor();
        let mu = (pos - k) as f32;
        let k = k as i64;
        let at = |o: i64| buf[((k + o) as usize) & mask];
        let (xm1, x0, x1, x2) = (at(-1), at(0), at(1), at(2));
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * mu + c2) * mu + c1) * mu + x0
    }

    fn forget(&mut self) {
        self.valid_from = self.t;
        self.expect = None;
        self.tail_left = 0;
        self.last_valid = false;
        self.prev_local = f64::MAX;
    }

    fn update_split(&mut self) {
        if self.sr <= 0.0 {
            return;
        }
        let lo = self.lo.clamp(20.0, 20_000.0);
        let hi = self.hi.clamp(20.0, 20_000.0).max(lo * 1.05);
        self.lo_on = lo > LO_OFF;
        self.hi_on = hi < HI_OFF && hi < self.sr * 0.45;
        for s in &mut self.split {
            s.set(self.sr, lo, hi);
        }
    }
}

impl Processor for HalfTime {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        // 8 bars at 50 BPM, 1/4 speed: 28.8 s of history
        let n = ((sr as f64 * 30.0) as usize).next_power_of_two();
        self.bl = vec![0.0; n];
        self.br = vec![0.0; n];
        self.mask = n - 1;
        self.w = 0;
        self.t = 0;
        if self.div == 0.0 {
            self.div = 2.0;
            self.loop_beats = 4.0;
            self.mix = 1.0;
            self.lo = 20.0;
            self.hi = 20_000.0;
            self.smooth = 10.0;
        }
        self.mix_s = self.mix;
        self.update_split();
        self.forget();
    }

    fn set_params(&mut self, p: &ParamBlock) {
        let v = &p.values;
        self.div = SPEED_DIV[(v[idx::SPEED].round() as usize).min(2)];
        self.loop_beats = LOOP_BEATS[(v[idx::LOOP].round() as usize).min(7)];
        self.smooth = v[idx::SMOOTH].clamp(-100.0, 100.0);
        let (lo, hi) = (v[idx::LO], v[idx::HI]);
        self.solo = v[idx::SOLO] >= 0.5;
        self.mix = (v[idx::MIX] / 100.0).clamp(0.0, 1.0);
        if lo != self.lo || hi != self.hi {
            self.lo = lo;
            self.hi = hi;
            self.update_split();
        }
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &ProcessCtx) {
        if self.bl.is_empty() {
            return;
        }
        let n = l.len();
        let sr = self.sr as f64;
        let playing = ctx.playing && ctx.bpm > 1.0;
        let spb = 60.0 * sr / ctx.bpm.max(1.0); // samples per beat
        if playing {
            if let Some(e) = self.expect {
                // a loop (song loop, pattern preview) that wraps on a window boundary keeps
                // the phase: only a jump that lands elsewhere in the window starts over
                let off = (ctx.beat - e).rem_euclid(self.loop_beats);
                if off > 1e-3 && off < self.loop_beats - 1e-3 {
                    self.forget();
                }
            } else {
                self.forget();
            }
            self.expect = Some(ctx.beat + n as f64 / spb);
        } else {
            self.expect = None;
        }
        let lb = self.loop_beats;
        let n_loop = lb * spb;
        let slope = 1.0 - 1.0 / self.div;
        let max_d = (self.mask as f64 - 8.0) - 0.02 * sr;
        let fade_ms = smooth_ms(self.smooth) as f64;
        let sustain = self.smooth >= 0.0;
        let fade = (fade_ms * 0.001 * sr).min(if sustain { 0.9 } else { 0.5 } * n_loop).max(1.0);
        let handover = (0.010 * sr) as u32;
        let mix_a = 1.0 - (-1.0 / (0.02 * self.sr)).exp();
        let (lo_on, hi_on, solo) = (self.lo_on, self.hi_on, self.solo);
        let mut ga_last = 1.0;
        let mut gb_last = 0.0;

        for i in 0..n {
            let (al, bl_) = self.split[0].tick(l[i], lo_on, hi_on);
            let (ar, br_) = self.split[1].tick(r[i], lo_on, hi_on);
            self.bl[self.w] = al;
            self.br[self.w] = ar;
            self.w = (self.w + 1) & self.mask;
            self.t += 1;

            let (mut wl, mut wr) = (al, ar);
            if playing {
                let beat = ctx.beat + i as f64 / spb;
                let local = beat.rem_euclid(lb) * spb;
                let wrapped = local < self.prev_local;
                self.prev_local = local;
                let d_a = (local * slope).min(max_d);
                // the window started at t - local: valid when that is inside continuous playback
                let start = self.t as f64 - local;
                let valid = start >= self.valid_from as f64 - 0.5;
                if valid {
                    // parameter change mid-window: hand the old trajectory to head B
                    if self.last_valid && !wrapped && (slope != self.last_slope || (d_a - self.last_d).abs() > 2.0) && self.tail_left == 0 {
                        self.tail_d = self.last_d + self.last_slope;
                        self.tail_slope = self.last_slope;
                        self.tail_len = handover.max(1);
                        self.tail_left = self.tail_len;
                    }
                    if wrapped && sustain && self.last_valid {
                        self.tail_d = self.last_d + self.last_slope;
                        self.tail_slope = self.last_slope;
                        self.tail_len = fade as u32;
                        self.tail_left = self.tail_len;
                    }
                    let a_l = Self::read(&self.bl, self.mask, self.w, d_a);
                    let a_r = Self::read(&self.br, self.mask, self.w, d_a);
                    let (mut ga, mut gb) = (1.0f32, 0.0f32);
                    let (mut b_l, mut b_r) = (0.0, 0.0);
                    if self.tail_left > 0 {
                        let u = 1.0 - self.tail_left as f32 / self.tail_len as f32;
                        let (s, c) = (u * std::f32::consts::FRAC_PI_2).sin_cos();
                        ga = s;
                        gb = c;
                        let d_b = self.tail_d.min(max_d);
                        b_l = Self::read(&self.bl, self.mask, self.w, d_b);
                        b_r = Self::read(&self.br, self.mask, self.w, d_b);
                        self.tail_d += self.tail_slope;
                        self.tail_left -= 1;
                    } else if !sustain {
                        let to_end = n_loop - local;
                        if to_end <= fade {
                            // percussive: fade into the input that the next window starts with
                            let u = ((fade - to_end) / fade) as f32;
                            let (s, c) = (u * std::f32::consts::FRAC_PI_2).sin_cos();
                            let d_in = (to_end * slope).min(max_d);
                            b_l = Self::read(&self.bl, self.mask, self.w, d_in);
                            b_r = Self::read(&self.br, self.mask, self.w, d_in);
                            ga = c;
                            gb = s;
                        }
                    }
                    wl = a_l * ga + b_l * gb;
                    wr = a_r * ga + b_r * gb;
                    ga_last = ga;
                    gb_last = gb;
                }
                self.last_d = d_a;
                self.last_slope = slope;
                self.last_valid = valid;
            }
            self.mix_s += (self.mix - self.mix_s) * mix_a;
            let m = self.mix_s;
            let keep = if solo { 0.0 } else { 1.0 };
            l[i] = (1.0 - m) * (al + bl_) + m * (wl + keep * bl_);
            r[i] = (1.0 - m) * (ar + br_) + m * (wr + keep * br_);
            if !playing {
                self.last_valid = false;
            }
        }
        self.m_phase = if playing { (ctx.beat + n as f64 / spb).rem_euclid(lb) as f32 / lb as f32 } else { 0.0 };
        self.m_active = if playing && self.last_valid { 1.0 } else { 0.0 };
        self.m_ga = ga_last;
        self.m_gb = gb_last;
        self.m_max_d = (n_loop * slope + fade) as f32;
    }

    fn reset(&mut self) {
        for s in &mut self.split {
            s.reset();
        }
        self.forget();
    }

    fn tail_samples(&self) -> Option<usize> {
        if self.bl.is_empty() {
            return Some(0);
        }
        // the whole window can still be read back after the input went silent
        Some(self.m_max_d.max(self.sr * 2.0) as usize + (0.05 * self.sr) as usize)
    }

    fn meters(&self, out: &mut [f32; METERS]) {
        out[0] = self.m_phase;
        out[1] = self.m_active;
        out[2] = self.m_ga;
        out[3] = self.m_gb;
        out[4] = self.div as f32;
        out[5] = self.loop_beats as f32;
        out[6] = if self.smooth >= 0.0 { smooth_ms(self.smooth) } else { -smooth_ms(self.smooth) };
        out[7] = if self.lo_on { self.lo } else { 0.0 };
        out[8] = if self.hi_on { self.hi } else { 0.0 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb(speed: f32, lp: f32, smooth: f32, lo: f32, hi: f32, mix: f32) -> ParamBlock {
        let mut p = ParamBlock::default();
        p.values[idx::SPEED] = speed;
        p.values[idx::LOOP] = lp;
        p.values[idx::SMOOTH] = smooth;
        p.values[idx::LO] = lo;
        p.values[idx::HI] = hi;
        p.values[idx::MIX] = mix;
        p
    }

    /// Runs `secs` of `f(sample index)` through the effect with the transport playing from beat 0.
    fn run(fx: &mut HalfTime, f: impl Fn(usize) -> f32, secs: f32, bpm: f64, playing: bool) -> Vec<f32> {
        let sr = 48_000.0;
        let n = (secs * sr) as usize;
        let mut out = Vec::with_capacity(n);
        let blk = 256;
        let mut i = 0;
        while i < n {
            let m = blk.min(n - i);
            let mut l: Vec<f32> = (i..i + m).map(&f).collect();
            let mut r = l.clone();
            let ctx = ProcessCtx { sample_rate: sr, bpm, beat: i as f64 / (60.0 * sr as f64 / bpm), playing };
            fx.process(&mut l, &mut r, &ctx);
            out.extend_from_slice(&l);
            i += m;
        }
        out
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn stopped_and_full_band_is_a_pass_through() {
        let mut fx = HalfTime::default();
        fx.prepare(48_000.0, 256);
        fx.set_params(&pb(1.0, 4.0, 10.0, 20.0, 20_000.0, 100.0));
        let sig = |i: usize| (i as f32 * 0.05).sin() * 0.5;
        let out = run(&mut fx, sig, 0.5, 120.0, false);
        assert!(out.iter().enumerate().all(|(i, &y)| (y - sig(i)).abs() < 1e-6));
    }

    #[test]
    fn two_x_halves_the_pitch_and_keeps_the_window_grid() {
        let mut fx = HalfTime::default();
        fx.prepare(48_000.0, 256);
        // 1/4 note windows at 120 BPM = 24000 samples
        fx.set_params(&pb(1.0, 2.0, 10.0, 20.0, 20_000.0, 100.0));
        let f = 440.0 / 48_000.0;
        let out = run(&mut fx, |i| (std::f32::consts::TAU * f * i as f32).sin() * 0.5, 2.0, 120.0, true);
        // the second window onward: 220 Hz instead of 440 Hz
        let w = &out[24_000 + 2000..48_000 - 2000];
        let hz = zero_crossings(w) as f32 / (w.len() as f32 / 48_000.0);
        assert!((hz - 220.0).abs() < 4.0, "{hz} Hz");
        // 4× → 110 Hz
        fx.set_params(&pb(2.0, 2.0, 10.0, 20.0, 20_000.0, 100.0));
        let out = run(&mut fx, |i| (std::f32::consts::TAU * f * i as f32).sin() * 0.5, 2.0, 120.0, true);
        let w = &out[24_000 + 2000..48_000 - 2000];
        let hz = zero_crossings(w) as f32 / (w.len() as f32 / 48_000.0);
        assert!((hz - 110.0).abs() < 4.0, "{hz} Hz");
    }

    #[test]
    fn window_boundaries_do_not_click() {
        for smooth in [10.0, -10.0, 60.0, -60.0] {
            let mut fx = HalfTime::default();
            fx.prepare(48_000.0, 256);
            fx.set_params(&pb(1.0, 1.0, smooth, 20.0, 20_000.0, 100.0));
            let f = 300.0 / 48_000.0;
            let out = run(&mut fx, |i| (std::f32::consts::TAU * f * i as f32).sin() * 0.5, 2.0, 120.0, true);
            // largest sample-to-sample step stays near the sine's own (2π·f·A ≈ 0.02)
            let step = out[1000..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
            assert!(step < 0.05, "smooth {smooth}: step {step}");
        }
    }

    #[test]
    fn a_split_band_sums_flat_when_stopped() {
        let mut fx = HalfTime::default();
        fx.prepare(48_000.0, 256);
        fx.set_params(&pb(1.0, 4.0, 10.0, 200.0, 4000.0, 100.0));
        let out = run(&mut fx, |i| if i == 0 { 1.0 } else { 0.0 }, 1.0, 120.0, false);
        // all-pass: energy of the impulse response stays 1
        let e: f32 = out.iter().map(|v| v * v).sum();
        assert!((e - 1.0).abs() < 0.02, "energy {e}");
    }

    #[test]
    fn a_jump_never_reads_audio_from_before_it() {
        let mut fx = HalfTime::default();
        fx.prepare(48_000.0, 256);
        fx.set_params(&pb(1.0, 4.0, 10.0, 20.0, 20_000.0, 100.0));
        // play 1 s of loud signal, then jump to the middle of a window with silence: the
        // remainder of that window must stay silent (no read-back of the loud part)
        let _ = run(&mut fx, |_| 0.8, 1.0, 120.0, true);
        let sr = 48_000.0f32;
        let spb = 24_000.0;
        let mut out = vec![];
        for b in 0..40 {
            let mut l = vec![0.0f32; 256];
            let mut r = l.clone();
            let ctx = ProcessCtx { sample_rate: sr, bpm: 120.0, beat: 10.5 + (b * 256) as f64 / spb, playing: true };
            fx.process(&mut l, &mut r, &ctx);
            out.extend_from_slice(&l);
        }
        assert!(out.iter().all(|v| v.abs() < 1e-6));
    }
}
