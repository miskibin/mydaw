//! Musical analysis of audio: tempo (BPM) and key.
//!
//! Offline only (never on the audio thread). One pass over a source produces a
//! [`SourceMusic`]: an onset envelope summary turned into a tempo estimate, and
//! a chroma (12 pitch classes) frame sequence that any region — a clip, or all
//! clips of a project — can be summed from cheaply to estimate a key.
//!
//! Tempo: log-band spectral flux → autocorrelation scored on a fine BPM grid
//! with its multiples (so the true period beats its halves/doubles) and a
//! log-normal preference around 120 BPM, then refined with a phase-locked comb
//! over the whole file, which resolves hundredths of a BPM on grid-quantised
//! music. Key: tuning-corrected spectral-peak chroma, each frame normalised so
//! loud sections do not outvote quiet ones, correlated with 24 key profiles.

use std::sync::Arc;

use realfft::RealFftPlanner;
use serde::Serialize;

pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Lowest/highest tempo the detector considers.
pub const MIN_BPM: f64 = 50.0;
pub const MAX_BPM: f64 = 240.0;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoCandidate {
    pub bpm: f64,
    /// Rhythmic evidence for this tempo without the 120-BPM preference, 0..1 (best = 1).
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoEstimate {
    pub bpm: f64,
    /// 0..1: how clearly one pulse stands out.
    pub confidence: f32,
    /// The pick first, then half and double time (when in range).
    pub candidates: Vec<TempoCandidate>,
    /// Seconds from the start of the audio to the first detected beat.
    pub first_beat: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyCandidate {
    /// Pitch class of the tonic, 0 = C.
    pub root: u8,
    pub minor: bool,
    /// Correlation with the key profile, −1..1.
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyEstimate {
    pub root: u8,
    pub minor: bool,
    /// 0..1: margin of the pick over the runner-up that is not the same key.
    pub confidence: f32,
    /// Best three keys, best first.
    pub candidates: Vec<KeyCandidate>,
    /// Pitch-class profile of the analysed audio, C..B, max = 1.
    pub chroma: [f32; 12],
    /// Detected tuning offset from A=440 in cents.
    pub tuning_cents: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MusicAnalysis {
    pub tempo: Option<TempoEstimate>,
    pub key: Option<KeyEstimate>,
}

/// Everything a source contributes to tempo and key analysis. Cached per source.
pub struct SourceMusic {
    pub tempo: Option<TempoEstimate>,
    /// Chroma frames (each max-normalised, or all zero when the frame is quiet).
    pub chroma: Vec<[f32; 12]>,
    /// Chroma frames per second.
    pub chroma_rate: f64,
    pub tuning_cents: f32,
}

impl SourceMusic {
    /// Sum of the chroma frames between `a` and `b` seconds, and how many frames had content.
    pub fn chroma_sum(&self, a: f64, b: f64) -> ([f32; 12], usize) {
        let mut s = [0f32; 12];
        let i0 = (a.max(0.0) * self.chroma_rate).floor() as usize;
        let i1 = ((b.max(0.0) * self.chroma_rate).ceil() as usize).min(self.chroma.len());
        let mut n = 0;
        for f in self.chroma.get(i0..i1.max(i0)).unwrap_or(&[]) {
            if f.iter().any(|v| *v > 0.0) {
                n += 1;
            }
            for k in 0..12 {
                s[k] += f[k];
            }
        }
        (s, n)
    }
}

/// Analyse mono audio at `sr`.
pub fn analyze(mono: &[f32], sr: u32) -> SourceMusic {
    let mut planner = RealFftPlanner::<f32>::new();
    let (env, fps) = onset_envelope(mono, sr, &mut planner);
    let tempo = estimate_tempo(&env, fps, &mut planner);
    let (chroma, chroma_rate, tuning_cents) = chroma_frames(mono, sr, &mut planner);
    SourceMusic {
        tempo,
        chroma,
        chroma_rate,
        tuning_cents,
    }
}

// ---------------------------------------------------------------- tempo

const ONSET_BANDS: usize = 36;

/// Log-band spectral flux, ~172 frames per second. Returns (envelope, frames per second).
pub fn onset_envelope(mono: &[f32], sr: u32, planner: &mut RealFftPlanner<f32>) -> (Vec<f32>, f64) {
    let sr_f = sr.max(1) as f64;
    let n_fft = if sr <= 50_000 { 2048 } else { 4096 };
    let hop = ((sr_f / 172.0).round() as usize).max(1);
    let fps = sr_f / hop as f64;
    if mono.len() < n_fft {
        return (Vec::new(), fps);
    }
    let fft = planner.plan_fft_forward(n_fft);
    let win = hann(n_fft);
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    // log-spaced bands 30 Hz .. 11 kHz
    let bin_hz = sr_f / n_fft as f64;
    let (lo, hi) = (30.0f64, 11_000.0f64.min(sr_f * 0.45));
    let edges: Vec<usize> = (0..=ONSET_BANDS)
        .map(|i| ((lo * (hi / lo).powf(i as f64 / ONSET_BANDS as f64)) / bin_hz).round() as usize)
        .collect();
    let frames = (mono.len() - n_fft) / hop + 1;
    let mut prev = [0f32; ONSET_BANDS];
    let mut env = Vec::with_capacity(frames);
    for t in 0..frames {
        let s = &mono[t * hop..t * hop + n_fft];
        for i in 0..n_fft {
            buf[i] = s[i] * win[i];
        }
        if fft
            .process_with_scratch(&mut buf, &mut spec, &mut scratch)
            .is_err()
        {
            env.push(0.0);
            continue;
        }
        let mut flux = 0f32;
        for b in 0..ONSET_BANDS {
            let (a, z) = (
                edges[b].max(1),
                edges[b + 1].max(edges[b].max(1) + 1).min(spec.len()),
            );
            let e: f32 =
                spec[a..z].iter().map(|c| c.norm_sqr()).sum::<f32>() / (z - a).max(1) as f32;
            let l = (1.0 + 1000.0 * e.sqrt() / n_fft as f32).ln();
            if t > 0 {
                flux += (l - prev[b]).max(0.0);
            }
            prev[b] = l;
        }
        env.push(flux);
    }
    // remove the slowly varying part (loudness changes), keep the pulses
    let w = (fps * 0.5) as usize | 1;
    let mean = moving_average(&env, w);
    for (e, m) in env.iter_mut().zip(mean) {
        *e = (*e - m).max(0.0);
    }
    (env, fps)
}

fn moving_average(x: &[f32], w: usize) -> Vec<f32> {
    let h = w / 2;
    let mut pre = Vec::with_capacity(x.len() + 1);
    pre.push(0f64);
    for v in x {
        pre.push(pre.last().unwrap() + *v as f64);
    }
    (0..x.len())
        .map(|i| {
            let a = i.saturating_sub(h);
            let b = (i + h + 1).min(x.len());
            ((pre[b] - pre[a]) / (b - a) as f64) as f32
        })
        .collect()
}

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
        .collect()
}

/// Unbiased autocorrelation of `x` for lags 0..max_lag, normalised so lag 0 = 1.
fn autocorr(x: &[f32], max_lag: usize, planner: &mut RealFftPlanner<f32>) -> Vec<f32> {
    let n = (x.len() * 2).next_power_of_two();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut a = fwd.make_input_vec();
    a[..x.len()].copy_from_slice(x);
    let mut s = fwd.make_output_vec();
    if fwd.process(&mut a, &mut s).is_err() {
        return vec![0.0; max_lag + 1];
    }
    for c in s.iter_mut() {
        *c = realfft::num_complex::Complex::new(c.norm_sqr(), 0.0);
    }
    let mut out = inv.make_output_vec();
    if inv.process(&mut s, &mut out).is_err() {
        return vec![0.0; max_lag + 1];
    }
    let z = out[0].max(1e-12);
    (0..=max_lag.min(x.len().saturating_sub(1)))
        .map(|l| out[l] / z * x.len() as f32 / (x.len() - l) as f32)
        .collect()
}

fn interp(v: &[f32], x: f64) -> f32 {
    if x < 0.0 {
        return 0.0;
    }
    let i = x as usize;
    if i + 1 >= v.len() {
        return 0.0;
    }
    let f = (x - i as f64) as f32;
    v[i] * (1.0 - f) + v[i + 1] * f
}

/// Rhythmic evidence for `bpm`: the autocorrelation at the beat period and its
/// multiples (a real pulse repeats every bar too), plus half the evidence at
/// the half period (eighth notes support the beat but cannot replace it).
fn periodicity(ac: &[f32], fps: f64, bpm: f64) -> f32 {
    let lag = 60.0 * fps / bpm;
    let mut s = 0.0;
    for (k, w) in [(1.0, 1.0), (2.0, 0.5), (3.0, 0.33), (4.0, 0.5)] {
        s += w * interp(ac, lag * k);
    }
    s + 0.25 * interp(ac, lag * 0.5)
}

/// Preference for tempos people tap to: log-normal around 120 BPM, about one octave wide.
fn tempo_prior(bpm: f64) -> f64 {
    let o = (bpm / 120.0).log2();
    (-0.5 * (o / 0.9).powi(2)).exp()
}

/// Mean onset strength on a beat grid of `period` frames starting at `phase`.
fn comb(env: &[f32], period: f64, phase: f64) -> f32 {
    let mut s = 0.0;
    let mut n = 0;
    let mut t = phase;
    while t < env.len() as f64 - 1.0 {
        s += interp(env, t);
        n += 1;
        t += period;
    }
    if n == 0 {
        0.0
    } else {
        s / n as f32
    }
}

/// Best phase (frames) and comb value for a period.
fn best_phase(env: &[f32], period: f64) -> (f64, f32) {
    let steps = (period.ceil() as usize).max(1);
    let mut best = (0.0, f32::MIN);
    for i in 0..steps * 2 {
        let ph = i as f64 * 0.5;
        if ph >= period {
            break;
        }
        let c = comb(env, period, ph);
        if c > best.1 {
            best = (ph, c);
        }
    }
    best
}

pub fn estimate_tempo(
    env: &[f32],
    fps: f64,
    planner: &mut RealFftPlanner<f32>,
) -> Option<TempoEstimate> {
    // need a few bars of material
    if env.len() < (fps * 4.0) as usize {
        return None;
    }
    let energy: f32 = env.iter().sum();
    if energy <= 1e-6 {
        return None;
    }
    // smooth slightly: beats are allowed to wander by a few milliseconds
    let env = {
        let k = [0.25f32, 0.5, 0.25];
        let mut o = env.to_vec();
        for i in 1..env.len() - 1 {
            o[i] = k[0] * env[i - 1] + k[1] * env[i] + k[2] * env[i + 1];
        }
        o
    };
    let max_lag = (60.0 * fps / MIN_BPM * 4.5) as usize + 2;
    let ac = autocorr(&env, max_lag, planner);
    // coarse: fine grid over the whole range
    let grid: Vec<f64> = (0..=((MAX_BPM - MIN_BPM) * 10.0) as usize)
        .map(|i| MIN_BPM + i as f64 * 0.1)
        .collect();
    let raw: Vec<f32> = grid.iter().map(|&b| periodicity(&ac, fps, b)).collect();
    let (bi, _) = grid
        .iter()
        .zip(&raw)
        .enumerate()
        .map(|(i, (&b, &r))| (i, r as f64 * tempo_prior(b)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    let coarse = grid[bi];
    let raw_max = raw.iter().copied().fold(f32::MIN, f32::max).max(1e-9);
    // confidence: how far the pick stands above the typical tempo hypothesis
    let mut sorted = raw.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    let confidence = ((raw[bi] - median) / raw[bi].abs().max(1e-9)).clamp(0.0, 1.0);

    // fine: phase-locked comb over the whole file, ±3 % around the coarse pick
    let mut best = (coarse, f32::MIN, 0.0);
    let mut b = coarse * 0.97;
    while b <= coarse * 1.03 {
        let (ph, c) = best_phase(&env, 60.0 * fps / b);
        if c > best.1 {
            best = (b, c, ph);
        }
        b += 0.01;
    }
    let (mut bpm, top, mut phase) = best;
    // produced music is nearly always on a whole BPM: prefer it when it explains the beats as well
    let whole = bpm.round();
    let (wph, wc) = best_phase(&env, 60.0 * fps / whole);
    if wc >= top * 0.985 {
        bpm = whole;
        phase = wph;
    } else {
        bpm = (bpm * 100.0).round() / 100.0;
    }
    let score = |b: f64| (periodicity(&ac, fps, b) / raw_max).clamp(0.0, 1.0);
    let mut candidates = vec![TempoCandidate {
        bpm,
        score: score(bpm),
    }];
    for alt in [bpm / 2.0, bpm * 2.0] {
        if (20.0..=999.0).contains(&alt) {
            let s = if (MIN_BPM..=MAX_BPM).contains(&alt) {
                score(alt)
            } else {
                0.0
            };
            candidates.push(TempoCandidate {
                bpm: (alt * 100.0).round() / 100.0,
                score: s,
            });
        }
    }
    Some(TempoEstimate {
        bpm,
        confidence,
        candidates,
        first_beat: phase / fps,
    })
}

// ---------------------------------------------------------------- key

/// Key profiles (major, minor), tonic first. Albrecht & Shanahan (2013), derived
/// from a corpus of tonal music; blended with the plain diatonic/triad shape so
/// the pick is robust on harmonically dense audio where overtones blur the fifth.
const MAJOR: [f32; 12] = [
    0.238, 0.006, 0.111, 0.006, 0.137, 0.094, 0.016, 0.214, 0.009, 0.080, 0.008, 0.081,
];
const MINOR: [f32; 12] = [
    0.220, 0.006, 0.104, 0.123, 0.019, 0.103, 0.012, 0.214, 0.062, 0.022, 0.061, 0.052,
];

/// Spectral-peak chroma frames: (frames, frames per second, tuning offset in cents).
pub fn chroma_frames(
    mono: &[f32],
    sr: u32,
    planner: &mut RealFftPlanner<f32>,
) -> (Vec<[f32; 12]>, f64, f32) {
    let sr_f = sr.max(1) as f64;
    let n_fft = ((sr_f * 0.18) as usize).next_power_of_two().max(1024);
    let hop = n_fft / 2;
    let rate = sr_f / hop as f64;
    if mono.len() < n_fft {
        return (Vec::new(), rate, 0.0);
    }
    let fft = planner.plan_fft_forward(n_fft);
    let win = hann(n_fft);
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let bin_hz = sr_f / n_fft as f64;
    let (k0, k1) = (
        ((60.0 / bin_hz) as usize).max(2),
        ((2100.0 / bin_hz) as usize).min(n_fft / 2 - 2),
    );
    let frames = (mono.len() - n_fft) / hop + 1;
    // first pass: spectral peaks (midi, weight) per frame and frame energy
    let mut peaks: Vec<Vec<(f32, f32)>> = Vec::with_capacity(frames);
    let mut energy = Vec::with_capacity(frames);
    let mut mag = vec![0f32; n_fft / 2 + 1];
    for t in 0..frames {
        let s = &mono[t * hop..t * hop + n_fft];
        for i in 0..n_fft {
            buf[i] = s[i] * win[i];
        }
        let mut ps = Vec::new();
        if fft
            .process_with_scratch(&mut buf, &mut spec, &mut scratch)
            .is_err()
        {
            peaks.push(ps);
            energy.push(0.0);
            continue;
        }
        for (m, c) in mag.iter_mut().zip(&spec) {
            *m = c.norm();
        }
        let mx = mag[k0..k1].iter().copied().fold(0f32, f32::max);
        energy.push(mag[k0..k1].iter().map(|m| m * m).sum::<f32>());
        let floor = mx * 0.05;
        for k in k0..k1 {
            let (a, b, c) = (mag[k - 1], mag[k], mag[k + 1]);
            if b > floor && b > a && b >= c {
                // parabolic interpolation on log magnitude
                let (la, lb, lc) = ((a + 1e-12).ln(), (b + 1e-12).ln(), (c + 1e-12).ln());
                let d = 0.5 * (la - lc) / (la - 2.0 * lb + lc).min(-1e-9);
                let f = (k as f64 + d as f64) * bin_hz;
                let midi = 69.0 + 12.0 * (f / 440.0).log2();
                ps.push((midi as f32, b));
            }
        }
        peaks.push(ps);
    }
    // tuning: circular mean of the peaks' offsets from the equal-tempered grid
    let (mut sx, mut sy) = (0f64, 0f64);
    for p in peaks.iter().flatten() {
        let th = std::f64::consts::TAU * p.0 as f64;
        sx += p.1 as f64 * th.cos();
        sy += p.1 as f64 * th.sin();
    }
    let tuning = if sx.abs() + sy.abs() > 0.0 {
        (sy.atan2(sx) / std::f64::consts::TAU) as f32
    } else {
        0.0
    };
    // quiet frames (below −50 dB of the loudest) carry no harmonic information
    let emax = energy.iter().copied().fold(0f32, f32::max);
    let gate = emax * 1e-5;
    let out = peaks
        .iter()
        .zip(&energy)
        .map(|(ps, &e)| {
            let mut c = [0f32; 12];
            if e <= gate || e <= 0.0 {
                return c;
            }
            for &(m, w) in ps {
                let m = m - tuning;
                let r = m.round();
                let d = m - r; // −0.5..0.5 semitones
                let shape = (std::f32::consts::PI * d).cos().powi(2);
                c[(r as i32).rem_euclid(12) as usize] += w * shape;
            }
            let mx = c.iter().copied().fold(0f32, f32::max);
            if mx > 0.0 {
                c.iter_mut().for_each(|v| *v /= mx);
            }
            c
        })
        .collect();
    (out, rate, tuning * 100.0)
}

fn pearson(x: &[f32; 12], p: &[f32; 12], shift: usize) -> f32 {
    let mx = x.iter().sum::<f32>() / 12.0;
    let mp = p.iter().sum::<f32>() / 12.0;
    let (mut s, mut a2, mut b2) = (0f32, 0f32, 0f32);
    for i in 0..12 {
        let a = x[i] - mx;
        let b = p[(i + 12 - shift) % 12] - mp;
        s += a * b;
        a2 += a * a;
        b2 += b * b;
    }
    if a2 <= 0.0 || b2 <= 0.0 {
        0.0
    } else {
        s / (a2 * b2).sqrt()
    }
}

/// Key of a pitch-class profile (any scale; only the shape matters).
pub fn estimate_key(chroma: &[f32; 12], tuning_cents: f32) -> Option<KeyEstimate> {
    let mx = chroma.iter().copied().fold(0f32, f32::max);
    if mx <= 0.0 || !mx.is_finite() {
        return None;
    }
    let mut norm = *chroma;
    norm.iter_mut().for_each(|v| *v /= mx);
    let mut all: Vec<KeyCandidate> = (0..12u8)
        .flat_map(|r| {
            [
                KeyCandidate {
                    root: r,
                    minor: false,
                    score: pearson(&norm, &MAJOR, r as usize),
                },
                KeyCandidate {
                    root: r,
                    minor: true,
                    score: pearson(&norm, &MINOR, r as usize),
                },
            ]
        })
        .collect();
    all.sort_by(|a, b| b.score.total_cmp(&a.score));
    let best = all[0];
    let second = all[1];
    let confidence = (((best.score - second.score) * 10.0).clamp(0.0, 1.0) * 0.5
        + best.score.clamp(0.0, 1.0) * 0.5)
        .clamp(0.0, 1.0);
    Some(KeyEstimate {
        root: best.root,
        minor: best.minor,
        confidence,
        candidates: all[..3].to_vec(),
        chroma: norm,
        tuning_cents,
    })
}

/// Human name like "A minor".
pub fn key_name(root: u8, minor: bool) -> String {
    format!(
        "{} {}",
        NOTE_NAMES[root as usize % 12],
        if minor { "minor" } else { "major" }
    )
}

/// Analysis of a whole source, ready to share.
pub fn analyze_audio(audio: &crate::sources::Audio) -> Arc<SourceMusic> {
    let mono: Vec<f32> = audio
        .content()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|f| (f[0] + f[1]) * 0.5)
        .collect();
    Arc::new(analyze(&mono, audio.sr))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    /// Decaying noise bursts on every beat (accented on 1) plus a hat on the off-beats.
    fn drums(bpm: f64, seconds: f64) -> Vec<f32> {
        let n = (seconds * SR as f64) as usize;
        let mut x = vec![0f32; n];
        let mut seed = 1u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        let spb = 60.0 / bpm;
        let mut k = 0;
        loop {
            let t0 = k as f64 * spb * 0.5;
            let i0 = (t0 * SR as f64) as usize;
            if i0 >= n {
                break;
            }
            let beat = k % 2 == 0;
            let amp = if k % 8 == 0 {
                1.0
            } else if beat {
                0.7
            } else {
                0.25
            };
            let len = if beat { 0.12 } else { 0.03 };
            for i in 0..((len * SR as f64) as usize).min(n - i0) {
                let env = (-(i as f32) / (len as f32 * SR as f32 * 0.25)).exp();
                let tone = if beat {
                    (std::f32::consts::TAU * 60.0 * i as f32 / SR as f32).sin()
                } else {
                    0.0
                };
                x[i0 + i] += amp * env * (0.5 * rnd() + tone);
            }
            k += 1;
        }
        x
    }

    fn tempo_of(x: &[f32]) -> TempoEstimate {
        let mut p = RealFftPlanner::new();
        let (env, fps) = onset_envelope(x, SR, &mut p);
        estimate_tempo(&env, fps, &mut p).expect("tempo")
    }

    #[test]
    fn finds_whole_bpm_tempos() {
        for bpm in [72.0, 90.0, 105.0, 128.0, 140.0, 160.0] {
            let t = tempo_of(&drums(bpm, 30.0));
            assert!((t.bpm - bpm).abs() < 0.05, "{bpm}: got {}", t.bpm);
            assert!(t.confidence > 0.3, "{bpm}: confidence {}", t.confidence);
        }
        // fast tempo (174 BPM) is either picked directly or offered as the 2× candidate of 87
        let t = tempo_of(&drums(174.0, 30.0));
        assert!(
            t.candidates.iter().any(|c| (c.bpm - 174.0).abs() < 0.1),
            "{:?}",
            t.candidates
        );
    }

    #[test]
    fn resolves_fractional_tempo() {
        let t = tempo_of(&drums(123.4, 40.0));
        assert!((t.bpm - 123.4).abs() < 0.06, "got {}", t.bpm);
    }

    #[test]
    fn offers_half_and_double_time() {
        let t = tempo_of(&drums(100.0, 20.0));
        let alts: Vec<f64> = t.candidates.iter().map(|c| c.bpm).collect();
        assert_eq!(alts, vec![100.0, 50.0, 200.0]);
    }

    #[test]
    fn first_beat_follows_the_audio() {
        let mut x = vec![0f32; (0.3 * SR as f64) as usize];
        x.extend(drums(120.0, 20.0));
        let t = tempo_of(&x);
        // beats every 0.5 s starting at 0.3 s (2048-sample FFT window centres ~23 ms before the onset)
        let ph = (t.first_beat - 0.3).rem_euclid(0.5);
        assert!(!(0.05..=0.45).contains(&ph), "first beat {}", t.first_beat);
    }

    #[test]
    fn silence_and_short_audio_have_no_tempo() {
        let mut p = RealFftPlanner::new();
        let (env, fps) = onset_envelope(&vec![0.0; SR as usize * 10], SR, &mut p);
        assert!(estimate_tempo(&env, fps, &mut p).is_none());
        let (env, fps) = onset_envelope(&drums(120.0, 2.0), SR, &mut p);
        assert!(estimate_tempo(&env, fps, &mut p).is_none());
    }

    /// Chords with harmonics; `detune` in cents.
    fn chords(prog: &[[i32; 3]], seconds_each: f64, detune: f32) -> Vec<f32> {
        let mut x = Vec::new();
        for ch in prog {
            let n = (seconds_each * SR as f64) as usize;
            for i in 0..n {
                let t = i as f32 / SR as f32;
                let mut v = 0.0;
                for &m in ch {
                    let f = 440.0 * 2f32.powf((m as f32 - 69.0 + detune / 100.0) / 12.0);
                    for h in 1..=5 {
                        v += (std::f32::consts::TAU * f * h as f32 * t).sin() * 0.6f32.powi(h - 1);
                    }
                }
                let env = (i as f32 / 400.0).min(1.0) * ((n - i) as f32 / 400.0).min(1.0);
                x.push(0.1 * v * env);
            }
        }
        x
    }

    fn key_of(x: &[f32]) -> KeyEstimate {
        let m = analyze(x, SR);
        let (s, _) = m.chroma_sum(0.0, 1e9);
        estimate_key(&s, m.tuning_cents).expect("key")
    }

    #[test]
    fn detects_major_and_minor_keys() {
        // C major: C F G C (I IV V I)
        let k = key_of(&chords(
            &[[60, 64, 67], [65, 69, 72], [67, 71, 74], [60, 64, 67]],
            1.5,
            0.0,
        ));
        assert_eq!((k.root, k.minor), (0, false), "{:?}", k.candidates);
        // A minor: Am Dm E Am (harmonic minor dominant)
        let k = key_of(&chords(
            &[[57, 60, 64], [62, 65, 69], [64, 68, 71], [57, 60, 64]],
            1.5,
            0.0,
        ));
        assert_eq!((k.root, k.minor), (9, true), "{:?}", k.candidates);
        // F# minor: i VI VII i (natural minor, the pop progression classical profiles miss)
        let k = key_of(&chords(
            &[[54, 57, 61], [50, 54, 57], [52, 56, 59], [54, 57, 61]],
            1.5,
            0.0,
        ));
        assert_eq!((k.root, k.minor), (6, true), "{:?}", k.candidates);
    }

    #[test]
    fn key_survives_detuned_recordings() {
        // a quarter tone flat would otherwise smear every note across two pitch classes
        let x = chords(
            &[[62, 66, 69], [67, 71, 74], [69, 73, 76], [62, 66, 69]],
            1.5,
            -40.0,
        );
        let m = analyze(&x, SR);
        assert!(
            (m.tuning_cents + 40.0).abs() < 8.0,
            "tuning {}",
            m.tuning_cents
        );
        let (s, _) = m.chroma_sum(0.0, 1e9);
        let k = estimate_key(&s, m.tuning_cents).unwrap();
        assert_eq!((k.root, k.minor), (2, false), "{:?}", k.candidates);
    }

    #[test]
    fn silent_audio_has_no_key() {
        let m = analyze(&vec![0.0; SR as usize * 5], SR);
        let (s, n) = m.chroma_sum(0.0, 1e9);
        assert_eq!(n, 0);
        assert!(estimate_key(&s, 0.0).is_none());
    }

    #[test]
    fn chroma_sum_respects_the_region() {
        // C major chord for 3 s, then F# major for 3 s
        let x = chords(&[[60, 64, 67], [66, 70, 73]], 3.0, 0.0);
        let m = analyze(&x, SR);
        let (a, _) = m.chroma_sum(0.0, 2.5);
        let (b, _) = m.chroma_sum(3.5, 6.0);
        assert!(a[0] > a[6] * 3.0 && b[6] > b[0] * 3.0, "{a:?} {b:?}");
    }
}
