//! A deterministic synthetic singer for testing and benchmarking pitch tools with
//! exactly known ground truth: syllables (a noise consonant, then a vowel shaped by
//! formants) on a melody, with vibrato, glides between notes, per-note detuning and
//! slow drift. Two `Voice`s singing the same `song` with different timing and
//! timbre stand in for a take and its reference.

use crate::pitch::{midi_to_hz, ANALYSIS_RATE};

/// F1, F2, F3 (Hz) of a, e, i, o, u for an adult male; scaled per voice.
const VOWELS: [[f32; 3]; 5] = [
    [730.0, 1090.0, 2440.0],
    [530.0, 1840.0, 2480.0],
    [270.0, 2290.0, 3010.0],
    [570.0, 840.0, 2410.0],
    [300.0, 870.0, 2240.0],
];

#[derive(Clone, Copy, Debug)]
pub struct Syllable {
    /// 0..5 = a e i o u
    pub vowel: usize,
    pub midi: f32,
    /// Nominal length in seconds (consonant included).
    pub dur: f64,
    /// Starts with an unvoiced consonant.
    pub consonant: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Voice {
    /// Formant frequencies × this (≈1.0 male, ≈1.18 female).
    pub formant: f32,
    pub vib_rate: f32,
    /// Vibrato depth, semitones (peak).
    pub vib_depth: f32,
    /// Tempo factor: durations × this.
    pub tempo: f64,
    /// Random timing jitter per syllable, ± fraction of its length.
    pub jitter: f64,
    /// Transposition, semitones.
    pub transpose: f32,
    pub seed: u32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            formant: 1.0,
            vib_rate: 5.5,
            vib_depth: 0.35,
            tempo: 1.0,
            jitter: 0.0,
            transpose: 0.0,
            seed: 1,
        }
    }
}

pub struct Rendition {
    pub audio: Vec<f32>,
    pub sr: f32,
    /// Intended pitch + the singer's own vibrato and glides (no detune, no drift), MIDI at
    /// `ANALYSIS_RATE`; NaN where unvoiced.
    pub truth: Vec<f32>,
    /// What was actually sung (truth + detune + drift).
    pub sung: Vec<f32>,
    /// Per syllable: (vowel onset, end) in seconds and the intended MIDI pitch.
    pub notes: Vec<(f64, f64, f32)>,
    /// Per syllable: start of its consonant (or vowel) in seconds.
    pub onsets: Vec<f64>,
    /// Frames within 40 ms of a glide or note edge (excluded from pitch scoring).
    pub edge: Vec<bool>,
}

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32
    }
    fn sym(&mut self) -> f32 {
        self.next() * 2.0 - 1.0
    }
}

/// Sings `song`. `detune[k]` (cents) offsets syllable k, `drift` (cents per second)
/// pulls every note away from its pitch as it is held.
pub fn sing(song: &[Syllable], v: &Voice, detune: &[f32], drift: f32, sr: f32) -> Rendition {
    let mut rng = Rng(v.seed.wrapping_mul(2654435761).max(1));
    // timing
    let mut t = 0.15f64;
    let mut segs = vec![]; // (consonant start, vowel start, end)
    for s in song {
        let d = s.dur * v.tempo * (1.0 + v.jitter * rng.sym() as f64);
        let cons = if s.consonant {
            0.05f64.min(d * 0.3)
        } else {
            0.0
        };
        segs.push((t, t + cons, t + d));
        t += d;
    }
    let total = ((t + 0.25) * sr as f64) as usize;
    let mut audio = vec![0f32; total];
    let frames = (total as f32 / sr * ANALYSIS_RATE) as usize + 1;
    let mut truth = vec![f32::NAN; frames];
    let mut sung = vec![f32::NAN; frames];
    let mut edge = vec![false; frames];
    let mut phase = 0f64;
    let mut env = 0f32;
    let mut noise_lp = 0f32;
    let mut prev_end_pitch: Option<f32> = None;
    for (k, s) in song.iter().enumerate() {
        let (c0, v0, e) = segs[k];
        let target = s.midi + v.transpose;
        let det = detune.get(k).copied().unwrap_or(0.0) / 100.0;
        let fm = VOWELS[s.vowel % 5].map(|f| f * v.formant);
        // consonant: filtered noise, unvoiced
        let (n0, n1) = ((c0 * sr as f64) as usize, (v0 * sr as f64) as usize);
        for (i, out) in audio.iter_mut().enumerate().take(n1).skip(n0) {
            let x = rng.sym();
            noise_lp += 0.6 * (x - noise_lp);
            let w = ((i as f64 / sr as f64 - c0) / (v0 - c0).max(1e-6)) as f32;
            *out += (x - noise_lp) * 0.12 * (std::f32::consts::PI * w).sin();
        }
        // vowel
        let glide_from = if s.consonant { None } else { prev_end_pitch };
        let (i0, i1) = (
            (v0 * sr as f64) as usize,
            ((e * sr as f64) as usize).min(total),
        );
        let vib_phase = rng.next() * std::f32::consts::TAU;
        for (i, out) in audio.iter_mut().enumerate().take(i1).skip(i0) {
            let tt = i as f64 / sr as f64;
            let since = (tt - v0) as f32;
            let left = (e - tt) as f32;
            let mut intended = target;
            if let Some(g) = glide_from {
                if since < 0.06 {
                    let w = since / 0.06;
                    intended = g + (target - g) * (w * w * (3.0 - 2.0 * w));
                }
            }
            let vib_amt = ((since - 0.15) / 0.2).clamp(0.0, 1.0);
            let vib = v.vib_depth
                * vib_amt
                * (std::f32::consts::TAU * v.vib_rate * since + vib_phase).sin();
            let pitch_true = intended + vib;
            let pitch_sung = pitch_true + det + drift / 100.0 * since;
            let hz = midi_to_hz(pitch_sung) as f64;
            phase = (phase + hz / sr as f64).fract();
            // amplitude envelope: 20 ms attack, 30 ms release
            let want = (since / 0.02).min(1.0).min(left / 0.03).max(0.0);
            env += (want - env) * 0.01;
            let nh = ((sr * 0.45) as f64 / hz) as usize;
            let mut y = 0f32;
            for h in 1..=nh.min(60) {
                let fh = hz as f32 * h as f32;
                let g = fm
                    .iter()
                    .zip([1.0f32, 0.7, 0.35])
                    .map(|(f, a)| a * (-((fh - f) / (60.0 + 0.08 * f)).powi(2)).exp())
                    .sum::<f32>()
                    + 0.015;
                y += g / (h as f32).sqrt()
                    * (std::f32::consts::TAU as f64 * phase * h as f64).sin() as f32;
            }
            *out += y * env * 0.12 + rng.sym() * 0.0015;
            let f = (tt * ANALYSIS_RATE as f64).round() as usize;
            if f < frames {
                truth[f] = pitch_true;
                sung[f] = pitch_sung;
                if since < 0.04 + if glide_from.is_some() { 0.06 } else { 0.0 } || left < 0.04 {
                    edge[f] = true;
                }
            }
        }
        prev_end_pitch = Some(target);
    }
    let notes = song
        .iter()
        .enumerate()
        .map(|(k, s)| (segs[k].1, segs[k].2, s.midi + v.transpose))
        .collect();
    let onsets = segs.iter().map(|s| s.0).collect();
    Rendition {
        audio,
        sr,
        truth,
        sung,
        notes,
        onsets,
        edge,
    }
}

/// |curve − truth| in cents over voiced, non-edge frames where both exist.
/// `offset` is the first frame of `curve` within `truth`.
pub fn pitch_errors(truth: &[f32], edge: &[bool], curve: &[f32], offset: usize) -> Vec<f32> {
    curve
        .iter()
        .enumerate()
        .filter_map(|(i, &c)| {
            let f = i + offset;
            let t = *truth.get(f)?;
            (!t.is_nan() && !c.is_nan() && !edge.get(f).copied().unwrap_or(true))
                .then(|| (c - t).abs() * 100.0)
        })
        .collect()
}

/// `q`-quantile (0..1) of `v`; NaN when empty.
pub fn quantile(v: &[f32], q: f32) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    s[((s.len() - 1) as f32 * q).round() as usize]
}

/// A short song: `bars` phrases of syllables over a melody in C major, deterministic in `seed`.
pub fn song(seed: u32, syllables: usize) -> Vec<Syllable> {
    let mut rng = Rng(seed.wrapping_mul(747796405).max(1));
    let scale = [0, 2, 4, 5, 7, 9, 11, 12, 14];
    let mut deg = 2usize;
    (0..syllables)
        .map(|k| {
            let step = (rng.next() * 5.0) as i32 - 2;
            deg = (deg as i32 + step).clamp(0, scale.len() as i32 - 1) as usize;
            let long = k % 4 == 3;
            Syllable {
                vowel: (rng.next() * 5.0) as usize % 5,
                midi: 55.0 + scale[deg] as f32,
                dur: if long {
                    0.7
                } else {
                    0.28 + 0.14 * (rng.next() as f64)
                },
                consonant: rng.next() < 0.7,
            }
        })
        .collect()
}
