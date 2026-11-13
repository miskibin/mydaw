//! RMVPE (Wei et al., 2023): a vocal pitch estimator robust to accompaniment and
//! breathy or rough voices, run through ONNX Runtime. Preprocessing and decoding
//! follow the RVC reference implementation exactly (`infer/rmvpe.py`): 16 kHz audio,
//! 128-band log-mel (n_fft 1024, hop 160, 30–8000 Hz, HTK mel scale with Slaney area
//! normalisation, magnitude, ln(max(x, 1e-5))), frames padded to a multiple of 32;
//! the model's 360 sigmoid bins (20 cents apart from 1997.38 cents) are decoded by a
//! weighted average of the 9 bins around the peak, unvoiced where the peak ≤ 0.03.

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use realfft::RealFftPlanner;
use wave_engine::pitch::{F0Estimator, F0Track};

pub const SR: f32 = 16000.0;
pub const HOP: usize = 160;
pub const RATE: f32 = SR / HOP as f32;
const N_FFT: usize = 1024;
const N_MELS: usize = 128;
const BINS: usize = 360;
const THRESHOLD: f32 = 0.03;
/// Frames per model call (a multiple of 32) and the context dropped at each inner edge.
const CHUNK: usize = 4096;
const OVERLAP: usize = 128;

/// The model is not shipped: the user downloads it on request after reading the
/// licence note. Swap these four to point at another build of the same network.
pub const MODEL_URL: &str = "https://huggingface.co/lj1995/VoiceConversionWebUI/resolve/main/rmvpe.onnx";
pub const MODEL_SHA256: &str = "5370e71ac80af8b4b7c793d27efd51fd8bf962de3a7ede0766dac0befa3660fd";
pub const MODEL_BYTES: u64 = 361_688_443;
pub const LICENSE_URL: &str = "https://huggingface.co/lj1995/VoiceConversionWebUI/blob/main/%E4%BD%BF%E7%94%A8%E9%9C%80%E9%81%B5%E5%AE%88%E7%9A%84%E5%8D%8F%E8%AE%AE-LICENSE.txt";
pub const LICENSE_NOTE: &str = "RMVPE weights from the RVC project (lj1995/VoiceConversionWebUI). The RVC code is MIT and the RMVPE \
reference code Apache-2.0, but the weights' licence file restricts use to research. Downloading is your decision; \
without the model WAVE uses its built-in YIN detector.";

/// Where the downloaded model lives (`WAVE_RMVPE_MODEL` overrides it).
pub fn model_path() -> PathBuf {
    if let Some(p) = std::env::var_os("WAVE_RMVPE_MODEL") {
        return PathBuf::from(p);
    }
    let scratch = wave_engine::engine::scratch_dir();
    scratch.parent().unwrap_or(&scratch).join("Models").join("rmvpe.onnx")
}

pub struct Rmvpe {
    session: Mutex<ort::session::Session>,
    _env: ort::environment::Environment,
    mel_basis: Vec<Vec<(usize, f32)>>,
}

impl Rmvpe {
    pub fn load(model: &Path) -> Result<Self, String> {
        let fail = |e: String| format!("RMVPE model could not be loaded: {e}");
        let env = ort::init().build().map_err(|e| fail(e.to_string()))?;
        let threads = std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4);
        let mut b = ort::session::Session::builder(&env)
            .map_err(|e| fail(e.to_string()))?
            .with_intra_threads(threads)
            .map_err(|e| fail(e.to_string()))?;
        let session = b.commit_from_file(model).map_err(|e| fail(e.to_string()))?;
        Ok(Self { session: Mutex::new(session), _env: env, mel_basis: mel_basis() })
    }

    /// Log-mel frames (`N_MELS` per frame) of 16 kHz audio.
    pub fn log_mel(&self, x: &[f32]) -> Vec<[f32; N_MELS]> {
        log_mel(x, &self.mel_basis)
    }

    /// Salience (`BINS` per frame) for log-mel frames.
    fn salience(&self, mel: &[[f32; N_MELS]]) -> Result<Vec<f32>, String> {
        let n = mel.len();
        let mut out = vec![0f32; n * BINS];
        let mut s = 0usize;
        while s < n {
            let a = s.saturating_sub(OVERLAP);
            let b = (s + CHUNK - 2 * OVERLAP).min(n);
            let e = (b + OVERLAP).min(n);
            let len = e - a;
            let padded = len.div_ceil(32) * 32;
            // [1, 128, T], zero padded on the right like RVC
            let mut data = vec![0f32; N_MELS * padded];
            for (t, fr) in mel[a..e].iter().enumerate() {
                for (m, v) in fr.iter().enumerate() {
                    data[m * padded + t] = *v;
                }
            }
            let input = ort::value::Tensor::from_array(([1usize, N_MELS, padded], data)).map_err(|e| e.to_string())?;
            let mut sess = self.session.lock();
            let res = sess.run(ort::inputs![input]).map_err(|e| format!("RMVPE inference failed: {e}"))?;
            let (_, h) = res[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
            for t in s..b {
                let k = t - a;
                out[t * BINS..(t + 1) * BINS].copy_from_slice(&h[k * BINS..(k + 1) * BINS]);
            }
            s = b;
        }
        Ok(out)
    }
}

impl F0Estimator for Rmvpe {
    fn name(&self) -> &'static str {
        "rmvpe"
    }

    fn estimate(&self, mono: &[f32], sr: f32) -> Result<F0Track, String> {
        let x = resample(mono, sr, SR);
        let mel = self.log_mel(&x);
        if mel.is_empty() {
            return Ok(F0Track { rate: RATE, hz: vec![], confidence: vec![] });
        }
        let sal = self.salience(&mel)?;
        let (hz, confidence) = decode(&sal);
        Ok(F0Track { rate: RATE, hz, confidence })
    }
}

fn hz_to_mel_htk(f: f64) -> f64 {
    2595.0 * (1.0 + f / 700.0).log10()
}
fn mel_to_hz_htk(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2595.0) - 1.0)
}

/// librosa.filters.mel(sr=16000, n_fft=1024, n_mels=128, fmin=30, fmax=8000, htk=True)
/// (norm='slaney'), as sparse rows.
fn mel_basis() -> Vec<Vec<(usize, f32)>> {
    let bins = N_FFT / 2 + 1;
    let fft: Vec<f64> = (0..bins).map(|k| k as f64 * SR as f64 / N_FFT as f64).collect();
    let (lo, hi) = (hz_to_mel_htk(30.0), hz_to_mel_htk(8000.0));
    let pts: Vec<f64> = (0..N_MELS + 2).map(|i| mel_to_hz_htk(lo + (hi - lo) * i as f64 / (N_MELS + 1) as f64)).collect();
    (0..N_MELS)
        .map(|i| {
            let (f0, f1, f2) = (pts[i], pts[i + 1], pts[i + 2]);
            let enorm = 2.0 / (f2 - f0);
            (0..bins)
                .filter_map(|k| {
                    let lower = (fft[k] - f0) / (f1 - f0);
                    let upper = (f2 - fft[k]) / (f2 - f1);
                    let w = lower.min(upper).max(0.0) * enorm;
                    (w > 0.0).then_some((k, w as f32))
                })
                .collect()
        })
        .collect()
}

/// torch.stft(center=True, pad_mode='reflect', periodic Hann 1024, hop 160) magnitude → mel → ln.
fn log_mel(x: &[f32], basis: &[Vec<(usize, f32)>]) -> Vec<[f32; N_MELS]> {
    if x.len() < 2 {
        return vec![];
    }
    let half = N_FFT / 2;
    let reflect = |i: i64| -> f32 {
        let n = x.len() as i64;
        let mut j = i;
        while j < 0 || j >= n {
            j = if j < 0 { -j } else { 2 * (n - 1) - j };
        }
        x[j as usize]
    };
    let frames = x.len() / HOP + 1;
    let hann: Vec<f32> = (0..N_FFT).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N_FFT as f32).cos()).collect();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut mag = vec![0f32; N_FFT / 2 + 1];
    (0..frames)
        .map(|f| {
            let s = (f * HOP) as i64 - half as i64;
            for (i, b) in buf.iter_mut().enumerate() {
                *b = reflect(s + i as i64) * hann[i];
            }
            let _ = fft.process_with_scratch(&mut buf, &mut spec, &mut scratch);
            for (m, c) in mag.iter_mut().zip(&spec) {
                *m = c.norm();
            }
            let mut out = [0f32; N_MELS];
            for (o, row) in out.iter_mut().zip(basis) {
                let v: f32 = row.iter().map(|&(k, w)| mag[k] * w).sum();
                *o = v.max(1e-5).ln();
            }
            out
        })
        .collect()
}

/// Peak bin → weighted average of the 9 bins around it (RVC `to_local_average_cents`).
pub fn decode(sal: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let n = sal.len() / BINS;
    let cents = |b: usize| 20.0 * b as f32 + 1997.3794;
    let mut hz = vec![0f32; n];
    let mut conf = vec![0f32; n];
    for t in 0..n {
        let s = &sal[t * BINS..(t + 1) * BINS];
        let (c, &mx) = s.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
        conf[t] = mx;
        if mx <= THRESHOLD {
            continue;
        }
        let (a, b) = (c.saturating_sub(4), (c + 5).min(BINS));
        let (mut ws, mut wc) = (0f32, 0f32);
        for (k, &v) in s.iter().enumerate().take(b).skip(a) {
            ws += v;
            wc += v * cents(k);
        }
        hz[t] = 10.0 * 2f32.powf(wc / ws / 1200.0);
    }
    (hz, conf)
}

/// Band-limited resampling (windowed sinc, Kaiser β = 8, 32 zero crossings).
pub fn resample(x: &[f32], from: f32, to: f32) -> Vec<f32> {
    if (from - to).abs() < 0.5 {
        return x.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let n = (x.len() as f64 * ratio).ceil() as usize;
    let cutoff = ratio.min(1.0) * 0.97;
    let zc = 32.0;
    let half = (zc / cutoff).ceil() as i64;
    let i0 = |v: f64| -> f64 {
        // modified Bessel function of the first kind, order 0
        let (mut s, mut t) = (1.0, 1.0);
        for k in 1..25 {
            t *= (v / 2.0) / k as f64;
            s += t * t;
        }
        s
    };
    let beta = 8.0;
    let norm = i0(beta);
    (0..n)
        .map(|j| {
            let c = j as f64 / ratio;
            let ci = c.floor() as i64;
            let mut acc = 0f64;
            for k in ci - half + 1..=ci + half {
                if k < 0 || k as usize >= x.len() {
                    continue;
                }
                let d = (c - k as f64) * cutoff;
                let r = d / zc;
                if r.abs() >= 1.0 {
                    continue;
                }
                let sinc = if d.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d) };
                let w = i0(beta * (1.0 - r * r).sqrt()) / norm;
                acc += x[k as usize] as f64 * sinc * w * cutoff;
            }
            acc as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_a_tone() {
        let x: Vec<f32> = (0..48000).map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin()).collect();
        let y = resample(&x, 48000.0, 16000.0);
        assert_eq!(y.len(), 16000);
        let want: Vec<f32> = (0..16000).map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 16000.0).sin()).collect();
        let err = y[500..15500].iter().zip(&want[500..15500]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(err < 0.01, "max error {err}");
    }

    #[test]
    fn decoding_reads_the_peak_between_bins() {
        let mut s = vec![0f32; BINS * 2];
        // frame 0: peak between bins 100 and 101 → cents 3997.38 + 10
        s[100] = 0.8;
        s[101] = 0.8;
        // frame 1: nothing above the threshold
        s[BINS + 50] = 0.02;
        let (hz, conf) = decode(&s);
        let want = 10.0 * 2f32.powf((20.0 * 100.5 + 1997.3794) / 1200.0);
        assert!((hz[0] / want - 1.0).abs() < 1e-4, "{} vs {want}", hz[0]);
        assert_eq!(hz[1], 0.0);
        assert!(conf[0] > 0.7);
    }

    #[test]
    fn mel_matches_the_reference_shape() {
        let b = mel_basis();
        assert_eq!(b.len(), N_MELS);
        assert!(b.iter().all(|r| !r.is_empty()), "every band has bins");
        let x: Vec<f32> = (0..16000).map(|i| (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin() * 0.2).collect();
        let m = log_mel(&x, &b);
        assert_eq!(m.len(), 101);
        // the band holding 220 Hz is far above the floor
        let band = (0..N_MELS).max_by(|&i, &j| m[50][i].total_cmp(&m[50][j])).unwrap();
        let (lo, hi) = (b[band].first().unwrap().0 as f32 * 15.625, b[band].last().unwrap().0 as f32 * 15.625);
        assert!(lo <= 220.0 && hi >= 220.0, "peak band {band} covers {lo}–{hi} Hz");
    }

    /// With the model at `RMVPE_MODEL` (not shipped): a 220 Hz voice reads as 220 Hz.
    #[test]
    fn model_finds_a_sung_pitch() {
        let Ok(p) = std::env::var("RMVPE_MODEL") else {
            return;
        };
        let m = Rmvpe::load(Path::new(&p)).unwrap();
        let sr = 44100.0;
        let x: Vec<f32> = (0..44100 * 2)
            .map(|i| (1..8).map(|k| (std::f32::consts::TAU * 220.0 * k as f32 * i as f32 / sr).sin() / k as f32).sum::<f32>() * 0.2)
            .collect();
        let t = m.estimate(&x, sr).unwrap();
        let mut v: Vec<f32> = t.hz[20..t.hz.len() - 20].to_vec();
        v.sort_by(f32::total_cmp);
        let med = v[v.len() / 2];
        assert!((1200.0 * (med / 220.0).log2()).abs() < 15.0, "median {med} Hz");
    }
}
