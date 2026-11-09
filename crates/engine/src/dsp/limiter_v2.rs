//! True-peak look-ahead brickwall limiter ("Limiter" built-in, v2).
//!
//! * Detection: polyphase oversampled true peak (ITU-R BS.1770-4 Annex 2 method; 4× always,
//!   8× near the ceiling, 384-tap Kaiser sinc) per channel, blended between per-channel and
//!   linked by "Stereo link".
//! * Gain computer: required gain `min(1, ceiling / peak)` → sliding minimum over the
//!   look-ahead window (+2 samples so the neighbours of a peak are covered too) → release
//!   → moving average over the look-ahead window. The average turns the gain change into
//!   a linear ramp that lands exactly when the peak arrives, so there is no hard knee in
//!   the gain (no clicks) and no clipper in the normal path (Giannoulis, Massberg & Reiss,
//!   "Digital dynamic range compressor design", JAES 2012; the min-hold + box-average
//!   look-ahead is the standard construction for a peak limiter with a guaranteed ceiling).
//! * Release: exponential; with "Auto" on it is program dependent — fast after isolated
//!   transients, up to 4× slower while the limiter is working continuously, which keeps
//!   bass and dense mixes from pumping or distorting.
//! * A last sample clamp at the ceiling only guards against rounding; the tests measure
//!   that it does not engage on real material.

// per-line / per-channel DSP loops index several parallel arrays at once
#![allow(clippy::needless_range_loop)]

use crate::processor::{ParamBlock, ProcessCtx, Processor};

pub mod idx {
    pub const GAIN: usize = 0;
    pub const CEIL: usize = 1;
    pub const REL: usize = 2;
    pub const AUTO: usize = 3;
    pub const LOOK: usize = 4;
    pub const LINK: usize = 5;
    pub const OUT: usize = 6;
}

/// Detector oversampling. The 4× phases always run; the odd 8× phases only when the 4× peak
/// is within 1.4 dB of the ceiling (4× can under-read near-Nyquist content by up to 0.7 dB).
const OS: usize = 8;
/// Taps per oversampling phase.
const TAPS: usize = 48;
/// Delay of the detector's phase 0 (the original sample) in input samples.
const DET_DELAY: usize = TAPS / 2;
pub const MAX_LOOK_MS: f32 = 5.0;
pub const MIN_LOOK_MS: f32 = 1.0;
/// Detection aims this far under the ceiling (dB) to absorb interpolation error.
const MARGIN_DB: f32 = 0.05;

fn look_samples(look_ms: f32, sr: f32) -> usize {
    ((look_ms.clamp(MIN_LOOK_MS, MAX_LOOK_MS) / 1000.0 * sr).round() as usize).max(4)
}

/// Delay the limiter adds (samples) at the given look-ahead, for latency compensation.
pub fn latency_samples(look_ms: f32, sr: f32) -> usize {
    look_samples(look_ms, sr) + DET_DELAY
}

/// 8× interpolation filter: Kaiser-windowed sinc, phases 1..7 (phase 0 is a pure delay).
fn design_fir() -> [[f32; TAPS]; OS - 1] {
    fn bessel_i0(x: f64) -> f64 {
        let (mut s, mut t) = (1.0, 1.0);
        for k in 1..30 {
            t *= (x / 2.0) / k as f64;
            s += t * t;
        }
        s
    }
    // 384 taps, centre at 192 = OS * DET_DELAY
    let centre = (OS * DET_DELAY) as f64;
    let beta = 10.0;
    let mut h = [[0f32; TAPS]; OS - 1];
    for p in 1..OS {
        let mut sum = 0f64;
        let mut row = [0f64; TAPS];
        for (j, v) in row.iter_mut().enumerate() {
            let m = (OS * j + p) as f64;
            let x = (m - centre) / OS as f64;
            let sinc = if x == 0.0 {
                1.0
            } else {
                (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
            };
            let r = (m - centre) / (centre + 1.0);
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta);
            *v = sinc * w;
            sum += *v;
        }
        // stored oldest → newest so it runs over a contiguous history slice
        for j in 0..TAPS {
            h[p - 1][TAPS - 1 - j] = (row[j] / sum) as f32;
        }
    }
    h
}

/// Monotonic-deque sliding minimum over a fixed-capacity ring (no allocation after `new`).
struct MinHold {
    val: Vec<f32>,
    at: Vec<u64>,
    head: usize,
    len: usize,
}
impl MinHold {
    fn new(cap: usize) -> Self {
        Self {
            val: vec![1.0; cap],
            at: vec![0; cap],
            head: 0,
            len: 0,
        }
    }
    #[inline]
    fn push(&mut self, n: u64, x: f32, window: usize) -> f32 {
        let cap = self.val.len();
        while self.len > 0 {
            let tail = (self.head + self.len - 1) % cap;
            if self.val[tail] >= x {
                self.len -= 1;
            } else {
                break;
            }
        }
        let t = (self.head + self.len) % cap;
        self.val[t] = x;
        self.at[t] = n;
        self.len += 1;
        while self.at[self.head] + window as u64 <= n {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.val[self.head]
    }
    fn clear(&mut self) {
        self.len = 0;
        self.head = 0;
    }
}

/// One channel's gain path.
struct Path {
    hold: MinHold,
    /// release envelope in f64: in f32 a slow release stalls a few ulps under unity
    rel: f64,
    hist: Vec<f32>,
    sum: f64,
}

pub struct LimiterV2 {
    sr: f32,
    fir: [[f32; TAPS]; OS - 1],
    /// input history, each sample written twice so the last TAPS are always contiguous
    x: [[f32; 2 * TAPS]; 2],
    xw: usize,
    gain: f32,
    ceil: f32,
    ceil_det: f32,
    out: f32,
    link: f32,
    rel_ms: f32,
    auto: bool,
    look: usize,
    look_ms: f32,
    k_rel: f32,
    k_avg: f32,
    gr_avg: f32,
    paths: [Path; 2],
    delay: [Vec<f32>; 2],
    dw: usize,
    n: u64,
    gr: f32,
    /// Samples the safety clamp had to touch since `prepare` (diagnostic; should stay 0).
    pub clamped: u64,
    v: [f32; 12],
}

impl Default for LimiterV2 {
    fn default() -> Self {
        let mut v = [0f32; 12];
        v[idx::CEIL] = -1.0;
        v[idx::REL] = 80.0;
        v[idx::AUTO] = 1.0;
        v[idx::LOOK] = 3.0;
        v[idx::LINK] = 100.0;
        let path = || Path {
            hold: MinHold::new(4),
            rel: 1.0,
            hist: vec![1.0; 4],
            sum: 4.0,
        };
        Self {
            sr: 0.0,
            fir: design_fir(),
            x: [[0.0; 2 * TAPS]; 2],
            xw: 0,
            gain: 1.0,
            ceil: 1.0,
            ceil_det: 1.0,
            out: 1.0,
            link: 1.0,
            rel_ms: 80.0,
            auto: true,
            look: 4,
            look_ms: 3.0,
            k_rel: 0.0,
            k_avg: 0.0,
            gr_avg: 0.0,
            paths: [path(), path()],
            delay: [vec![0.0; 4], vec![0.0; 4]],
            dw: 0,
            n: 0,
            gr: 0.0,
            clamped: 0,
            v,
        }
    }
}

impl LimiterV2 {
    fn apply(&mut self) {
        let v = self.v;
        self.gain = super::db_to_gain(v[idx::GAIN].clamp(0.0, 24.0));
        self.ceil = super::db_to_gain(v[idx::CEIL].clamp(-12.0, 0.0));
        self.ceil_det = self.ceil * super::db_to_gain(-MARGIN_DB);
        self.out = super::db_to_gain(v[idx::OUT].clamp(-12.0, 0.0));
        self.link = v[idx::LINK].clamp(0.0, 100.0) / 100.0;
        self.rel_ms = v[idx::REL].clamp(10.0, 1000.0);
        self.auto = v[idx::AUTO] >= 0.5;
        let look_ms = v[idx::LOOK].clamp(MIN_LOOK_MS, MAX_LOOK_MS);
        if self.sr > 0.0 {
            self.k_avg = 1.0 - (-1.0 / (self.sr * 0.4)).exp();
            let look = look_samples(look_ms, self.sr);
            if look != self.look || look_ms != self.look_ms {
                self.look_ms = look_ms;
                if look != self.look {
                    self.look = look;
                    self.reset();
                }
            }
        }
    }

    #[inline]
    fn true_peak(&self, c: usize) -> f32 {
        // the last TAPS input samples, oldest first
        let lin = &self.x[c][self.xw..self.xw + TAPS];
        let phase = |p: usize| -> f32 {
            // 8 independent accumulators so the dot product vectorises
            let mut acc = [0f32; 8];
            for (h, x) in self.fir[p - 1].as_chunks::<8>().0.iter().zip(lin.as_chunks::<8>().0) {
                for k in 0..8 {
                    acc[k] += h[k] * x[k];
                }
            }
            acc.iter().sum::<f32>().abs()
        };
        let mut pk = lin[TAPS - 1 - DET_DELAY].abs();
        for p in [2, 4, 6] {
            pk = pk.max(phase(p));
        }
        if pk > self.ceil_det * 0.85 {
            for p in [1, 3, 5, 7] {
                pk = pk.max(phase(p));
            }
        }
        pk
    }
}

impl Processor for LimiterV2 {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        let cap = look_samples(MAX_LOOK_MS, sr) + 4;
        for p in self.paths.iter_mut() {
            p.hold = MinHold::new(cap + 2);
            p.hist = vec![1.0; cap];
        }
        let dl = cap + DET_DELAY + 2;
        self.delay = [vec![0.0; dl], vec![0.0; dl]];
        self.look = look_samples(self.v[idx::LOOK], sr);
        self.look_ms = self.v[idx::LOOK];
        self.apply();
        self.reset();
    }

    fn set_params(&mut self, p: &ParamBlock) {
        self.v = p.values;
        self.apply();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        if self.sr <= 0.0 || self.delay[0].len() < 4 {
            return;
        }
        let w = self.look;
        let hold_w = w + 2;
        let dl = self.delay[0].len();
        let hl = self.paths[0].hist.len();
        let inv_w = 1.0 / w as f64;
        let mut g_min = 1f32;
        // program-dependent release: deeper sustained reduction → up to 4× slower
        let sustain = if self.auto {
            (-self.gr_avg / 6.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let rel = self.rel_ms * (1.0 + 3.0 * sustain);
        self.k_rel = 1.0 - (-1.0 / (self.sr * rel / 1000.0)).exp();
        for i in 0..l.len() {
            let a = [l[i] * self.gain, r[i] * self.gain];
            for (c, &v) in a.iter().enumerate() {
                self.x[c][self.xw] = v;
                self.x[c][self.xw + TAPS] = v;
            }
            self.xw = (self.xw + 1) % TAPS;
            let pk = [self.true_peak(0), self.true_peak(1)];
            let mx = pk[0].max(pk[1]);
            let n = self.n;
            self.n += 1;
            let mut gains = [1f32; 2];
            for (c, p) in self.paths.iter_mut().enumerate() {
                let pe = self.link * mx + (1.0 - self.link) * pk[c];
                let req = if pe > self.ceil_det {
                    self.ceil_det / pe
                } else {
                    1.0
                };
                let held = p.hold.push(n, req, hold_w);
                let held = held as f64;
                p.rel = if held < p.rel {
                    held
                } else {
                    held + (p.rel - held) * (1.0 - self.k_rel as f64)
                };
                if held - p.rel < 1e-7 {
                    // settled: land exactly, so a processed silence ends where `park` puts it
                    p.rel = held;
                }
                // moving average over the look-ahead window
                let slot = (n % hl as u64) as usize;
                let old = p.hist[(slot + hl - w) % hl];
                let g = p.rel as f32;
                p.hist[slot] = g;
                p.sum += g as f64 - old as f64;
                gains[c] = (p.sum * inv_w) as f32;
            }
            // the detector output belongs to the sample DET_DELAY back; delay audio by that + look
            for c in 0..2 {
                self.delay[c][self.dw] = a[c];
            }
            let rd = (self.dw + dl - (DET_DELAY + w)) % dl;
            self.dw = (self.dw + 1) % dl;
            let (oa, ob) = (self.delay[0][rd] * gains[0], self.delay[1][rd] * gains[1]);
            if oa.abs() > self.ceil || ob.abs() > self.ceil {
                self.clamped += 1;
            }
            l[i] = oa.clamp(-self.ceil, self.ceil) * self.out;
            r[i] = ob.clamp(-self.ceil, self.ceil) * self.out;
            let g = gains[0].min(gains[1]);
            g_min = g_min.min(g);
        }
        self.gr = super::gain_to_db(g_min).min(0.0);
        // slow (0.4 s) average of the block's deepest reduction drives the auto release
        let k = 1.0 - (1.0 - self.k_avg).powi(l.len() as i32);
        self.gr_avg += (self.gr - self.gr_avg) * k;
        if self.gr_avg > -1e-4 {
            self.gr_avg = 0.0;
        }
    }

    fn reset(&mut self) {
        self.x = [[0.0; 2 * TAPS]; 2];
        self.xw = 0;
        for p in self.paths.iter_mut() {
            p.hold.clear();
            p.rel = 1.0;
            p.hist.fill(1.0);
            p.sum = self.look as f64;
        }
        for d in self.delay.iter_mut() {
            d.fill(0.0);
        }
        self.dw = 0;
        self.n = 0;
        self.gr_avg = 0.0;
        self.gr = 0.0;
    }

    fn tail_samples(&self) -> Option<usize> {
        if self.sr <= 0.0 {
            return Some(0);
        }
        // audio still in the look-ahead delay, then the gain has to recover from the deepest
        // reduction: 17 time constants of the slowest (auto) release ≈ −140 dB of gain error
        let rel = self.rel_ms * if self.auto { 4.0 } else { 1.0 };
        let recover = 17.0 * rel / 1000.0 * self.sr;
        Some(self.delay[0].len() + self.look + recover as usize)
    }

    fn gain_reduction_db(&self) -> f32 {
        self.gr
    }
}

#[cfg(test)]
mod tests {
    use super::super::db_to_gain;
    use super::*;
    use crate::catalog::{new_fx, param_block};
    use crate::model::FxKind;

    const BLK: usize = 128;

    fn make(gain: f32, sr: f32) -> LimiterV2 {
        let mut pb = param_block(&new_fx(1, FxKind::Limit, 120.0));
        pb.values[idx::GAIN] = gain;
        let mut p = LimiterV2::default();
        p.prepare(sr, BLK);
        p.set_params(&pb);
        p
    }

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                s as f32 / u32::MAX as f32 * 2.0 - 1.0
            })
            .collect()
    }

    fn run(p: &mut LimiterV2, l: &[f32], r: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let (mut a, mut b) = (l.to_vec(), r.to_vec());
        for (x, y) in a.chunks_mut(BLK).zip(b.chunks_mut(BLK)) {
            p.process(x, y, &ProcessCtx::default());
        }
        (a, b)
    }

    /// 16× oversampled peak (Kaiser β 12 sinc, ±32 taps), independent of the detector's 8× filter.
    fn tp(x: &[f32]) -> f32 {
        let i0 = |x: f64| {
            let (mut s, mut t) = (1.0, 1.0);
            for k in 1..40 {
                t *= (x / 2.0) / k as f64;
                s += t * t;
            }
            s
        };
        let mut h = vec![[0f32; 64]; 16];
        for (k, row) in h.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                let t = (j as f64 - 32.0) + k as f64 / 16.0;
                let r = t / 33.0;
                let sinc = if t == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                };
                *v = (sinc * i0(12.0 * (1.0 - r * r).max(0.0).sqrt()) / i0(12.0)) as f32;
            }
        }
        let mut pk = 0f32;
        for i in 32..x.len() - 32 {
            for row in &h {
                let v: f32 = row.iter().enumerate().map(|(j, c)| c * x[i + 32 - j]).sum();
                pk = pk.max(v.abs());
            }
        }
        pk
    }

    /// Steep low-pass at 0.42·fs: what a mastered file contains (nothing is right at Nyquist).
    fn band_limit(x: &[f32], sr: f32) -> Vec<f32> {
        let mut f = [super::super::Biquad::default(); 4];
        for (k, b) in f.iter_mut().enumerate() {
            b.set(
                super::super::FilterType::Lowpass,
                sr,
                0.42 * sr,
                [0.51, 0.6, 0.9, 2.56][k],
                0.0,
            );
        }
        x.iter()
            .map(|&v| f.iter_mut().fold(v, |a, b| b.tick(a)))
            .collect()
    }

    #[test]
    fn true_peak_ceiling_holds_on_hard_signals() {
        let ceil = db_to_gain(-1.0);
        for sr in [44100.0, 48000.0, 96000.0] {
            let n = (sr * 1.0) as usize;
            let nz = band_limit(&noise(n, 3), sr);
            let sigs: Vec<Vec<f32>> = vec![
                // fs/4 at 45°: the samples read 3 dB under the true peak
                (0..n)
                    .map(|i| (std::f32::consts::FRAC_PI_2 * i as f32 + 0.785).sin() * 0.9)
                    .collect(),
                // bursts of full-band noise with hard onsets
                (0..n)
                    .map(|i| if (i / 2000) % 2 == 0 { nz[i] } else { 0.0 })
                    .collect(),
                // square wave (Gibbs overshoot) and a single spike
                (0..n)
                    .map(|i| if (i / 37) % 2 == 0 { 0.9 } else { -0.9 })
                    .collect(),
                (0..n).map(|i| if i == n / 2 { 1.0 } else { 0.0 }).collect(),
            ];
            for (k, x) in sigs.iter().enumerate() {
                let mut p = make(12.0, sr);
                let r: Vec<f32> = x.iter().map(|v| -v * 0.5).collect();
                let (l, r) = run(&mut p, x, &r);
                assert!(
                    l.iter().chain(&r).all(|v| v.is_finite() && v.abs() <= ceil),
                    "sample over"
                );
                let pk = tp(&l).max(tp(&r));
                assert!(
                    pk <= ceil * db_to_gain(0.02),
                    "sr {sr} signal {k}: true peak {:.3} dBTP",
                    20.0 * pk.log10()
                );
                assert_eq!(p.clamped, 0, "the safety clamp had to act on signal {k}");
                assert!(p.gain_reduction_db() <= 0.0);
            }
        }
    }

    #[test]
    fn latency_is_what_it_reports_and_quiet_audio_passes_untouched() {
        let sr = 48000.0;
        let mut p = make(0.0, sr);
        let n = 4800;
        let x: Vec<f32> = (0..n).map(|i| if i == 100 { 0.5 } else { 0.0 }).collect();
        let (l, _) = run(&mut p, &x, &x);
        let lat = latency_samples(3.0, sr);
        assert_eq!(l.iter().position(|v| v.abs() > 0.0), Some(100 + lat));
        assert!(
            (l[100 + lat] - 0.5).abs() < 1e-6,
            "below the ceiling the limiter is transparent"
        );
        assert_eq!(
            crate::catalog::latency(&new_fx(1, FxKind::Limit, 120.0), sr),
            lat
        );
    }

    #[test]
    fn low_distortion_on_a_sustained_bass_note() {
        // 100 Hz at 6 dB over the ceiling: the release must not ride the waveform
        let sr = 48000.0;
        let n = (sr * 2.0) as usize;
        let x: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 100.0 * i as f32 / sr).sin() * 1.8)
            .collect();
        let mut p = make(0.0, sr);
        let (l, _) = run(&mut p, &x, &x);
        // compare the last 0.5 s to a pure sine of the same amplitude
        let s = &l[n - 24000..];
        let amp = s.iter().fold(0f32, |m, v| m.max(v.abs()));
        let lat = latency_samples(3.0, sr);
        let err: f32 = s
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let t = (n - 24000 + i - lat) as f32;
                (v - amp / 1.8 * x[t as usize]).powi(2)
            })
            .sum::<f32>();
        let sig: f32 = s.iter().map(|v| v * v).sum();
        let thd_n = (err / sig).sqrt();
        assert!(thd_n < 0.005, "THD+N {:.3} %", thd_n * 100.0);
        assert!(p.gain_reduction_db() < -5.0 && p.gain_reduction_db() > -7.5);
    }

    #[test]
    fn parking_equals_processing_silence() {
        let sr = 48000.0;
        let mut a = make(10.0, sr);
        let mut b = make(10.0, sr);
        let gap = b.tail_samples().unwrap() / BLK * BLK + 10 * BLK;
        let burst = 24000 / BLK * BLK;
        let nz = noise(burst, 21);
        let mut diff = 0f32;
        let mut frame = 0;
        let mut parked = false;
        while frame < 2 * burst + gap {
            let loud = frame < burst || frame >= burst + gap;
            let mut la = [0f32; BLK];
            if loud {
                la.copy_from_slice(&nz[frame % burst..frame % burst + BLK]);
            }
            let (mut ra, mut lb, mut rb) = (la, la, la);
            a.process(&mut la, &mut ra, &ProcessCtx::default());
            if !loud && frame >= burst + gap - 2 * BLK {
                if !parked {
                    b.park();
                    parked = true;
                }
            } else {
                b.process(&mut lb, &mut rb, &ProcessCtx::default());
                for i in 0..BLK {
                    diff = diff.max((la[i] - lb[i]).abs()).max((ra[i] - rb[i]).abs());
                }
            }
            frame += BLK;
        }
        assert!(diff < 1e-6, "parked limiter differs by {diff}");
    }
}
