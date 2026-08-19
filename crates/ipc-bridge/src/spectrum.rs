//! Non-RT FFT of a [`SpectrumHop`](mydaw_rt_core::SpectrumHop).
//!
//! The audio thread only copies samples. This is where those samples become the
//! 128 log-spaced bins the EQ and delay displays draw, plus the downsampled
//! L/R traces the goniometer wants. Nothing here is touched from the callback.

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde::Serialize;
use std::sync::Arc;

use mydaw_rt_core::{SpectrumHop, SPECTRUM_HOP};

/// Log-spaced magnitude bins from 20 Hz to 20 kHz, matching the EQ's X axis.
pub const SPECTRUM_BINS: usize = 128;

/// Downsampled L/R pairs for the goniometer.
pub const SCOPE_POINTS: usize = 256;

/// Floor drawn for empty bins, in dBFS.
const FLOOR_DB: f32 = -90.0;

const MIN_HZ: f32 = 20.0;
const MAX_HZ: f32 = 20_000.0;

/// One analysed hop, as the webview consumes it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpectrumFrame {
    pub bins_pre: Vec<f32>,
    pub bins_post: Vec<f32>,
    pub scope_l: Vec<f32>,
    pub scope_r: Vec<f32>,
    pub correlation: f32,
}

/// Reusable FFT scratch. Built once; [`analyse`](Self::analyse) never allocates
/// beyond filling the output vectors the caller is about to serialise anyway.
pub struct SpectrumAnalyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    scratch: Vec<Complex<f32>>,
    window: Vec<f32>,
    window_scale: f32,
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(SPECTRUM_HOP);
        let window = hann(SPECTRUM_HOP);
        let window_sum: f32 = window.iter().sum();
        // Amplitude scaling: a full-scale sine reads 1.0. Hann's coherent gain
        // is 0.5, so 2/sum(window) == 4/N.
        let window_scale = if window_sum > 0.0 {
            2.0 / window_sum
        } else {
            0.0
        };
        Self {
            input: fft.make_input_vec(),
            scratch: fft.make_output_vec(),
            fft,
            window,
            window_scale,
        }
    }

    pub fn analyse(&mut self, hop: &SpectrumHop) -> SpectrumFrame {
        let sample_rate = if hop.sample_rate.is_finite() && hop.sample_rate > 0.0 {
            hop.sample_rate
        } else {
            48_000.0
        };
        SpectrumFrame {
            bins_pre: self.log_bins(&hop.pre_l, &hop.pre_r, sample_rate),
            bins_post: self.log_bins(&hop.post_l, &hop.post_r, sample_rate),
            scope_l: downsample(&hop.post_l),
            scope_r: downsample(&hop.post_r),
            correlation: pearson(&hop.post_l, &hop.post_r),
        }
    }

    fn log_bins(
        &mut self,
        left: &[f32; SPECTRUM_HOP],
        right: &[f32; SPECTRUM_HOP],
        sr: f32,
    ) -> Vec<f32> {
        for i in 0..SPECTRUM_HOP {
            let mono = 0.5 * (left[i] + right[i]);
            self.input[i] = mono * self.window[i];
        }
        self.fft
            .process(&mut self.input, &mut self.scratch)
            .expect("FFT size is fixed at construction");

        let bin_hz = sr / SPECTRUM_HOP as f32;
        let nyquist = sr * 0.5;
        let ratio = MAX_HZ / MIN_HZ;
        (0..SPECTRUM_BINS)
            .map(|i| {
                let t = i as f32 / (SPECTRUM_BINS - 1) as f32;
                let hz = MIN_HZ * ratio.powf(t);
                if hz >= nyquist {
                    return FLOOR_DB;
                }
                let pos = hz / bin_hz;
                let lo = pos.floor() as usize;
                let hi = (lo + 1).min(self.scratch.len().saturating_sub(1));
                let frac = pos - lo as f32;
                let mag_lo =
                    self.scratch[lo.min(self.scratch.len() - 1)].norm() * self.window_scale;
                let mag_hi = self.scratch[hi].norm() * self.window_scale;
                let mag = mag_lo + (mag_hi - mag_lo) * frac.clamp(0.0, 1.0);
                if mag > 1e-10 {
                    (20.0 * mag.log10()).clamp(FLOOR_DB, 12.0)
                } else {
                    FLOOR_DB
                }
            })
            .collect()
    }
}

impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

fn hann(n: usize) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
        .collect()
}

fn downsample(samples: &[f32; SPECTRUM_HOP]) -> Vec<f32> {
    let step = SPECTRUM_HOP / SCOPE_POINTS;
    (0..SCOPE_POINTS).map(|i| samples[i * step]).collect()
}

fn pearson(left: &[f32; SPECTRUM_HOP], right: &[f32; SPECTRUM_HOP]) -> f32 {
    let n = SPECTRUM_HOP as f32;
    let mut sum_l = 0.0_f32;
    let mut sum_r = 0.0_f32;
    let mut sum_ll = 0.0_f32;
    let mut sum_rr = 0.0_f32;
    let mut sum_lr = 0.0_f32;
    for i in 0..SPECTRUM_HOP {
        let l = left[i];
        let r = right[i];
        sum_l += l;
        sum_r += r;
        sum_ll += l * l;
        sum_rr += r * r;
        sum_lr += l * r;
    }
    let num = n * sum_lr - sum_l * sum_r;
    let den_l = n * sum_ll - sum_l * sum_l;
    let den_r = n * sum_rr - sum_r * sum_r;
    if den_l <= 0.0 || den_r <= 0.0 {
        return 0.0;
    }
    (num / (den_l.sqrt() * den_r.sqrt())).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_scale_1k_tone_peaks_near_1k() {
        let mut hop = SpectrumHop::SILENT;
        hop.sample_rate = 48_000.0;
        for i in 0..SPECTRUM_HOP {
            let x = (std::f32::consts::TAU * 1_000.0 * i as f32 / 48_000.0).sin();
            hop.post_l[i] = x;
            hop.post_r[i] = x;
            hop.pre_l[i] = x * 0.5;
            hop.pre_r[i] = x * 0.5;
        }
        let mut analyzer = SpectrumAnalyzer::new();
        let frame = analyzer.analyse(&hop);
        // Bin nearest 1 kHz on a 20 Hz…20 kHz log axis.
        let ratio = MAX_HZ / MIN_HZ;
        let t = (1_000.0 / MIN_HZ).ln() / ratio.ln();
        let bin = (t * (SPECTRUM_BINS - 1) as f32).round() as usize;
        let peak = frame.bins_post[bin];
        assert!(
            peak > -6.0,
            "1 kHz should be visible on the post curve, got {peak} dB in bin {bin}"
        );
        assert!(
            frame.bins_pre[bin] < peak - 3.0,
            "pre is 6 dB down, so its bin should sit clearly below post"
        );
        assert!((frame.correlation - 1.0).abs() < 0.02);
        assert_eq!(frame.scope_l.len(), SCOPE_POINTS);
        assert_eq!(frame.bins_post.len(), SPECTRUM_BINS);
    }
}
