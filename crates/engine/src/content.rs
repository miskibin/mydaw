//! Content features for aligning two performances of the same song: what is being
//! sung (vowels, consonants), largely independent of pitch and loudness. MFCCs at
//! 100 frames per second, normalised per recording (cepstral mean and variance
//! normalisation), so a different singer or microphone shifts them less.

use realfft::RealFftPlanner;

pub const CONTENT_RATE: f32 = 100.0;
pub const N_MFCC: usize = 12;
const N_MEL: usize = 40;

pub struct ContentFeatures {
    pub rate: f32,
    /// `N_MFCC` coefficients per frame (c1..c12; c0 = loudness is left out).
    pub mfcc: Vec<f32>,
    /// Frame energy, dB.
    pub energy: Vec<f32>,
}

impl ContentFeatures {
    pub fn frames(&self) -> usize {
        self.energy.len()
    }
    pub fn frame(&self, i: usize) -> &[f32] {
        &self.mfcc[i * N_MFCC..(i + 1) * N_MFCC]
    }
}

fn mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}
fn mel_inv(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

/// MFCCs of a mono signal. Frame `i` is centred on `i / CONTENT_RATE` seconds.
pub fn mfcc(mono: &[f32], sr: f32) -> ContentFeatures {
    let hop = sr as f64 / CONTENT_RATE as f64;
    let win = ((sr * 0.032) as usize).max(64);
    let nfft = win.next_power_of_two();
    let frames = (mono.len() as f64 / hop) as usize + 1;
    let hann: Vec<f32> = (0..win)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / win as f32).cos())
        .collect();
    // triangular mel filters on the power spectrum
    let bins = nfft / 2 + 1;
    let (lo, hi) = (mel(80.0), mel((sr * 0.5 * 0.95).min(7600.0)));
    let edges: Vec<f32> = (0..N_MEL + 2)
        .map(|k| mel_inv(lo + (hi - lo) * k as f32 / (N_MEL + 1) as f32) * nfft as f32 / sr)
        .collect();
    let filt: Vec<Vec<(usize, f32)>> = (0..N_MEL)
        .map(|m| {
            let (a, b, c) = (edges[m], edges[m + 1], edges[m + 2]);
            (a.floor() as usize..=(c.ceil() as usize).min(bins - 1))
                .filter_map(|k| {
                    let x = k as f32;
                    let w = if x <= b {
                        (x - a) / (b - a).max(1e-6)
                    } else {
                        (c - x) / (c - b).max(1e-6)
                    };
                    (w > 0.0).then_some((k, w))
                })
                .collect()
        })
        .collect();
    let dct: Vec<f32> = (1..=N_MFCC)
        .flat_map(|k| {
            (0..N_MEL).map(move |m| {
                (std::f32::consts::PI * k as f32 * (m as f32 + 0.5) / N_MEL as f32).cos()
            })
        })
        .collect();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(nfft);
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut out = vec![0f32; frames * N_MFCC];
    let mut energy = vec![0f32; frames];
    let mut logmel = [0f32; N_MEL];
    for f in 0..frames {
        let c = (f as f64 * hop) as i64;
        let s0 = c - win as i64 / 2;
        buf.fill(0.0);
        let mut e = 0f64;
        for i in 0..win {
            let k = s0 + i as i64;
            if k >= 0 && (k as usize) < mono.len() {
                let x = mono[k as usize];
                e += (x * x) as f64;
                buf[i] = x * hann[i];
            }
        }
        energy[f] = 10.0 * ((e / win as f64) + 1e-12).log10() as f32;
        let _ = fft.process_with_scratch(&mut buf, &mut spec, &mut scratch);
        for (m, fl) in filt.iter().enumerate() {
            let p: f32 = fl.iter().map(|&(k, w)| spec[k].norm_sqr() * w).sum();
            logmel[m] = (p + 1e-10).ln();
        }
        let o = &mut out[f * N_MFCC..(f + 1) * N_MFCC];
        for (k, ok) in o.iter_mut().enumerate() {
            *ok = dct[k * N_MEL..(k + 1) * N_MEL]
                .iter()
                .zip(&logmel)
                .map(|(a, b)| a * b)
                .sum();
        }
    }
    // normalise over the audible frames (silence would pull the mean toward its own spectrum)
    let top = energy.iter().copied().fold(f32::MIN, f32::max);
    let active: Vec<usize> = (0..frames).filter(|&f| energy[f] > top - 45.0).collect();
    if active.len() > 4 {
        for k in 0..N_MFCC {
            let mean =
                active.iter().map(|&f| out[f * N_MFCC + k]).sum::<f32>() / active.len() as f32;
            let var = active
                .iter()
                .map(|&f| (out[f * N_MFCC + k] - mean).powi(2))
                .sum::<f32>()
                / active.len() as f32;
            let sd = var.sqrt().max(1e-3);
            for f in 0..frames {
                out[f * N_MFCC + k] = (out[f * N_MFCC + k] - mean) / sd;
            }
        }
    }
    ContentFeatures {
        rate: CONTENT_RATE,
        mfcc: out,
        energy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vowels_differ_and_frames_line_up_with_time() {
        let sr = 48000.0;
        // two "vowels": the same 150 Hz source through different formant pairs
        let vowel = |f1: f32, f2: f32, n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    let t = i as f32 / sr;
                    (1..40)
                        .map(|k| {
                            let h = 150.0 * k as f32;
                            let g = (-((h - f1) / 120.0).powi(2)).exp()
                                + 0.6 * (-((h - f2) / 160.0).powi(2)).exp()
                                + 0.02;
                            g * (std::f32::consts::TAU * h * t).sin()
                        })
                        .sum::<f32>()
                        * 0.05
                })
                .collect()
        };
        let mut x = vowel(700.0, 1200.0, 24000);
        x.extend(vowel(300.0, 2300.0, 24000));
        let c = mfcc(&x, sr);
        assert_eq!(c.frames(), 101); // 1 s at 100 frames/s, frame 0 centred on t = 0
        let d = |a: usize, b: usize| {
            c.frame(a)
                .iter()
                .zip(c.frame(b))
                .map(|(p, q)| (p - q).powi(2))
                .sum::<f32>()
                .sqrt()
        };
        assert!(
            d(20, 30) * 3.0 < d(20, 70),
            "same vowel {} vs other vowel {}",
            d(20, 30),
            d(20, 70)
        );
    }
}
