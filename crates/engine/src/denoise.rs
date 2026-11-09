//! Spectral noise reduction (hiss, hum, room rumble, fan noise) for recorded vocals.
//!
//! Short-time Fourier transform, 2048 points at 44.1/48 kHz, 75 % overlap,
//! sqrt-Hann analysis and synthesis windows. The noise profile is learned on
//! the fly: per bin, the floor follows the smoothed power down at once, tracks
//! its mean while it stays within 6 dB (the first half second is learned
//! as-is) (pauses, noise between harmonics) and
//! only creeps up (3 dB/s) under louder signal, so a changing room is followed. Each bin is then attenuated in
//! proportion to how close it sits to that floor (Wiener gain on a
//! decision-directed a-priori SNR, with over-subtraction), never below the `reduction` floor. The gain is smoothed
//! across neighbouring bins and over time, which is what keeps the
//! "musical noise" (twinkling leftover tones) of plain spectral subtraction away.
//!
//! One mask is computed from both channels together and applied to each, so
//! the stereo image stays put. `Hold` freezes the learned profile; `Listen`
//! plays what is being removed.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::processor::{ParamBlock, ProcessCtx, Processor, METERS};

/// FFT size for a sample rate (about 43 ms).
pub fn fft_size(sr: f32) -> usize {
    if sr > 64_000.0 {
        4096
    } else {
        2048
    }
}

/// Samples of delay the effect adds (for latency compensation).
pub fn latency(sr: f32) -> usize {
    fft_size(sr)
}

const BANDS: usize = 16;

pub struct Denoise {
    sr: f32,
    n: usize,
    hop: usize,
    lat: usize,
    fwd: Option<Arc<dyn RealToComplex<f32>>>,
    inv: Option<Arc<dyn ComplexToReal<f32>>>,
    win: Vec<f32>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    acc_l: Vec<f32>,
    acc_r: Vec<f32>,
    rover: usize,
    tmp: Vec<f32>,
    spec_l: Vec<Complex<f32>>,
    spec_r: Vec<Complex<f32>>,
    scratch_f: Vec<Complex<f32>>,
    scratch_i: Vec<Complex<f32>>,
    /// smoothed power, learned noise floor, current gain (per bin)
    psm: Vec<f32>,
    noise: Vec<f32>,
    gain: Vec<f32>,
    graw: Vec<f32>,
    clean: Vec<f32>,
    frames: u32,
    learn_frames: u32,
    norm: f32,
    rise: f32,
    floor: f32,
    beta: f32,
    smooth: f32,
    hold: bool,
    listen: bool,
    edges: [usize; BANDS + 1],
    gr: f32,
    meter: [f32; METERS],
    params: Option<ParamBlock>,
    /// Samples of digital silence at the input; after a second the effect idles (state kept)
    /// until sound returns, so a skipped chain (`Processor::park`) changes nothing.
    quiet: usize,
    dormant: bool,
}

impl Default for Denoise {
    fn default() -> Self {
        Self {
            sr: 0.0,
            n: 0,
            hop: 0,
            lat: 0,
            fwd: None,
            inv: None,
            win: vec![],
            in_l: vec![],
            in_r: vec![],
            out_l: vec![],
            out_r: vec![],
            acc_l: vec![],
            acc_r: vec![],
            rover: 0,
            tmp: vec![],
            spec_l: vec![],
            spec_r: vec![],
            scratch_f: vec![],
            scratch_i: vec![],
            psm: vec![],
            noise: vec![],
            gain: vec![],
            graw: vec![],
            clean: vec![],
            frames: 0,
            learn_frames: 0,
            norm: 1.0,
            rise: 1.0,
            floor: 0.25,
            beta: 4.0,
            smooth: 0.5,
            hold: false,
            listen: false,
            edges: [0; BANDS + 1],
            gr: 0.0,
            meter: [0.0; METERS],
            params: None,
            quiet: 0,
            dormant: true,
        }
    }
}

impl Denoise {
    fn apply(&mut self) {
        let Some(p) = self.params else { return };
        let v = p.values;
        self.floor = 10f32.powf(-v[0].clamp(0.0, 40.0) / 20.0);
        // sensitivity: over-subtraction 0 … +10 dB above the learned floor
        self.beta = 10f32.powf(v[1].clamp(0.0, 100.0) / 100.0);
        self.smooth = (v[2] / 100.0).clamp(0.0, 1.0);
        self.hold = v[3] > 0.5;
        self.listen = v[4] > 0.5;
    }

    fn frame(&mut self) {
        let (Some(fwd), Some(inv)) = (self.fwd.clone(), self.inv.clone()) else { return };
        let n = self.n;
        let bins = n / 2 + 1;
        for (src, spec) in [(&self.in_l, &mut self.spec_l), (&self.in_r, &mut self.spec_r)] {
            for k in 0..n {
                self.tmp[k] = src[k] * self.win[k];
            }
            let _ = fwd.process_with_scratch(&mut self.tmp, spec, &mut self.scratch_f);
        }
        let first = self.frames == 0;
        let learning = self.frames < self.learn_frames;
        self.frames = self.frames.saturating_add(1);
        let (mut p_in, mut p_out) = (0f32, 0f32);
        for k in 0..bins {
            let p = (self.spec_l[k].norm_sqr() + self.spec_r[k].norm_sqr()) * 0.5 * self.norm;
            if first {
                self.psm[k] = p;
                self.noise[k] = p.max(1e-14);
            } else {
                self.psm[k] = 0.7 * self.psm[k] + 0.3 * p;
            }
            if !self.hold {
                let s = self.psm[k];
                let nz = self.noise[k];
                if learning {
                    // first half second: learn the room as it is
                    self.noise[k] = nz + (s - nz) * 0.25;
                } else if s < nz {
                    // below the floor: follow down quickly
                    self.noise[k] = 0.7 * nz + 0.3 * s;
                } else if s < 4.0 * nz {
                    // noise-like (within 6 dB): track its mean
                    self.noise[k] = nz + (s - nz) * 0.05;
                } else {
                    // signal: the floor may only creep up
                    self.noise[k] = nz * self.rise;
                }
                self.noise[k] = self.noise[k].max(1e-14);
            }
            // decision-directed a-priori SNR (Ephraim–Malah): mostly last frame's
            // cleaned estimate, a little of the raw excess → no twinkling leftovers
            let nz = self.noise[k];
            let post = p / nz;
            let prio = if first { (post - 1.0).max(0.0) } else { 0.95 * self.clean[k] / nz + 0.05 * (post - 1.0).max(0.0) };
            let g = (prio / (prio + self.beta)).clamp(self.floor, 1.0);
            self.clean[k] = g * g * p;
            self.graw[k] = g;
            p_in += p;
        }
        // smoothing across frequency (wider when `smooth` is up) …
        let w = 1 + (self.smooth * 3.0) as usize;
        for k in 0..bins {
            let (a, b) = (k.saturating_sub(w), (k + w).min(bins - 1));
            let mut s = 0f32;
            for j in a..=b {
                s += self.graw[j];
            }
            let g = (s / (b - a + 1) as f32).max(self.graw[k]).min(1.0);
            // … and over time: open fast (attacks stay intact), close a bit slower
            let kk = if g > self.gain[k] { 0.9 } else { 0.7 - 0.4 * self.smooth };
            self.gain[k] += (g - self.gain[k]) * kk;
            let gg = if self.listen { 1.0 - self.gain[k] } else { self.gain[k] };
            self.spec_l[k] *= gg;
            self.spec_r[k] *= gg;
            p_out += (self.spec_l[k].norm_sqr() + self.spec_r[k].norm_sqr()) * 0.5 * self.norm;
        }
        // DC and Nyquist must stay real for the inverse transform
        self.spec_l[0].im = 0.0;
        self.spec_r[0].im = 0.0;
        self.spec_l[bins - 1].im = 0.0;
        self.spec_r[bins - 1].im = 0.0;
        // overlap-add: periodic sqrt-Hann² at 75 % overlap sums to 2, FFT unnormalised (×n)
        let scale = 1.0 / (2.0 * n as f32);
        for (spec, acc) in [(&mut self.spec_l, &mut self.acc_l), (&mut self.spec_r, &mut self.acc_r)] {
            let _ = inv.process_with_scratch(spec, &mut self.tmp, &mut self.scratch_i);
            for k in 0..n {
                acc[k] += self.tmp[k] * self.win[k] * scale;
            }
        }
        let hop = self.hop;
        self.out_l[..hop].copy_from_slice(&self.acc_l[..hop]);
        self.out_r[..hop].copy_from_slice(&self.acc_r[..hop]);
        for b in [&mut self.acc_l, &mut self.acc_r] {
            b.copy_within(hop.., 0);
            b[n - hop..].fill(0.0);
        }
        for b in [&mut self.in_l, &mut self.in_r] {
            b.copy_within(hop.., 0);
        }
        // meters: noise floor and gain per band, total reduction
        let gr = if p_in > 1e-12 { 10.0 * (p_out.max(1e-14) / p_in).log10() } else { 0.0 };
        self.gr = if self.listen { 0.0 } else { gr.clamp(-60.0, 0.0) };
        for b in 0..BANDS {
            let (a, e) = (self.edges[b], self.edges[b + 1].max(self.edges[b] + 1).min(bins));
            let (mut ns, mut gs) = (0f32, 0f32);
            for k in a..e {
                ns += self.noise[k];
                gs += self.gain[k];
            }
            let c = (e - a).max(1) as f32;
            self.meter[b] = 10.0 * (ns / c).max(1e-14).log10();
            self.meter[BANDS + b] = 20.0 * (gs / c).max(1e-6).log10();
        }
    }
}

impl Processor for Denoise {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        let n = fft_size(sr);
        self.n = n;
        self.hop = n / 4;
        self.lat = n - self.hop;
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        self.spec_l = fwd.make_output_vec();
        self.spec_r = fwd.make_output_vec();
        self.scratch_f = fwd.make_scratch_vec();
        self.scratch_i = inv.make_scratch_vec();
        self.fwd = Some(fwd);
        self.inv = Some(inv);
        self.win = (0..n).map(|k| (0.5 - 0.5 * (std::f32::consts::TAU * k as f32 / n as f32).cos()).sqrt()).collect();
        let wsum: f32 = self.win.iter().sum();
        self.norm = (2.0 / wsum) * (2.0 / wsum);
        for b in [&mut self.in_l, &mut self.in_r, &mut self.out_l, &mut self.out_r, &mut self.acc_l, &mut self.acc_r, &mut self.tmp] {
            *b = vec![0.0; n];
        }
        let bins = n / 2 + 1;
        self.psm = vec![0.0; bins];
        self.noise = vec![1e-14; bins];
        self.gain = vec![1.0; bins];
        self.graw = vec![1.0; bins];
        self.clean = vec![0.0; bins];
        // noise floor may climb back 3 dB per second
        let hop_s = self.hop as f32 / sr;
        self.rise = 10f32.powf(0.3 * hop_s);
        self.learn_frames = (0.5 / hop_s) as u32;
        for b in 0..=BANDS {
            let f = 50.0 * (16000.0f32 / 50.0).powf(b as f32 / BANDS as f32);
            self.edges[b] = ((f / sr * n as f32).round() as usize).min(bins);
        }
        self.rover = self.lat;
        self.frames = 0;
        self.quiet = 0;
        self.dormant = true;
        self.apply();
    }

    fn set_params(&mut self, p: &ParamBlock) {
        self.params = Some(*p);
        self.apply();
    }

    fn reset(&mut self) {
        for b in [&mut self.in_l, &mut self.in_r, &mut self.out_l, &mut self.out_r, &mut self.acc_l, &mut self.acc_r] {
            b.fill(0.0);
        }
        // keep the learned noise profile: a transport jump does not change the room
        self.gain.fill(1.0);
        self.rover = self.lat;
    }

    fn park(&mut self) {
        self.dormant = true;
    }

    fn tail_samples(&self) -> Option<usize> {
        // one FFT frame of latency plus the gain smoothing; the learned profile is kept
        Some(self.n + (self.sr * 1.5) as usize)
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        if self.fwd.is_none() {
            return;
        }
        let silent = l.iter().all(|&x| x == 0.0) && r.iter().all(|&x| x == 0.0);
        if silent {
            if self.dormant {
                return;
            }
            if self.quiet >= self.sr as usize {
                self.dormant = true;
                return;
            }
            self.quiet += l.len();
        } else {
            self.quiet = 0;
            self.dormant = false;
        }
        for i in 0..l.len() {
            self.in_l[self.rover] = l[i];
            self.in_r[self.rover] = r[i];
            l[i] = self.out_l[self.rover - self.lat];
            r[i] = self.out_r[self.rover - self.lat];
            self.rover += 1;
            if self.rover >= self.n {
                self.rover = self.lat;
                self.frame();
            }
        }
    }

    fn gain_reduction_db(&self) -> f32 {
        self.gr
    }

    /// 0‥16: learned noise floor per band (dB, 50 Hz – 16 kHz log-spaced);
    /// 16‥32: gain applied per band (dB, ≤ 0).
    fn meters(&self, out: &mut [f32; METERS]) {
        *out = self.meter;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{new_fx, param_block};
    use crate::model::FxKind;

    fn rng(seed: &mut u32) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        (*seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn run(d: &mut Denoise, x: &[f32]) -> Vec<f32> {
        let mut out = vec![];
        for c in x.chunks(256) {
            let (mut l, mut r) = (c.to_vec(), c.to_vec());
            d.process(&mut l, &mut r, &ProcessCtx::default());
            out.extend(l);
        }
        out
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn removes_noise_in_pauses_and_keeps_the_tone() {
        let sr = 48000.0;
        let mut d = Denoise::default();
        d.prepare(sr, 256);
        let mut fx = new_fx(1, FxKind::Denoise, 120.0);
        fx.params.insert("reduction".into(), 18.0);
        d.set_params(&param_block(&fx));
        let mut seed = 1u32;
        // 2 s noise only, then 1 s tone + noise
        let n = (sr * 3.0) as usize;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                let tone = if t > 2.0 { 0.3 * (std::f32::consts::TAU * 440.0 * t).sin() } else { 0.0 };
                tone + 0.02 * rng(&mut seed)
            })
            .collect();
        let y = run(&mut d, &x);
        let lat = latency(sr);
        let noise_in = rms(&x[(sr * 1.0) as usize..(sr * 1.9) as usize]);
        let noise_out = rms(&y[(sr * 1.0) as usize + lat..(sr * 1.9) as usize + lat]);
        let red = 20.0 * (noise_out / noise_in).log10();
        assert!(red < -12.0, "noise only reduced by {red:.1} dB");
        let tone_in = rms(&x[(sr * 2.3) as usize..(sr * 2.9) as usize]);
        let tone_out = rms(&y[(sr * 2.3) as usize + lat..(sr * 2.9) as usize + lat]);
        let keep = 20.0 * (tone_out / tone_in).log10();
        assert!(keep.abs() < 1.0, "tone level changed by {keep:.1} dB");
        assert!(d.gain_reduction_db() > -3.0);
    }

    #[test]
    fn transparent_on_clean_signal_and_latency_matches() {
        let sr = 44100.0;
        let mut d = Denoise::default();
        d.prepare(sr, 256);
        let mut fx = new_fx(1, FxKind::Denoise, 120.0);
        fx.params.insert("reduction".into(), 0.0);
        d.set_params(&param_block(&fx));
        let mut x = vec![0f32; 8192];
        x[100] = 1.0;
        let y = run(&mut d, &x);
        let pk = y.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap();
        assert_eq!(pk.0, 100 + latency(sr));
        assert!((pk.1 - 1.0).abs() < 0.02, "impulse came out at {}", pk.1);
    }
}
