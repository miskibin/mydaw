//! Monophonic pitch: detection (YIN on a 4× decimated stream) and pitch shifting
//! (TD-PSOLA, which keeps the formants of a voice where they are).
//! Shared by the real-time corrector and the offline note editor.

use crate::dsp::{Biquad, FilterType};

pub const MIN_HZ: f32 = 60.0;
pub const MAX_HZ: f32 = 1100.0;

pub fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}
pub fn midi_to_hz(m: f32) -> f32 {
    440.0 * 2f32.powf((m - 69.0) / 12.0)
}

/// Streaming YIN. Feed samples at the device rate; read `hz` / `confidence`.
pub struct Detector {
    dec: usize,
    lp1: Biquad,
    lp2: Biquad,
    phase: usize,
    buf: Vec<f32>,
    w: usize,
    win: usize,
    tau_min: usize,
    tau_max: usize,
    /// allocated maximum lag (`tau_max` can be narrowed by `set_range`)
    tau_cap: usize,
    hop: usize,
    since: usize,
    d: Vec<f32>,
    frame: Vec<f32>,
    pub hz: f32,
    pub confidence: f32,
    pub rms: f32,
    /// Pitch the caller expects (Hz, 0 = none): in frames without a clear dip the
    /// dip nearest to it wins over a louder harmonic's.
    pub hint: f32,
    sr_dec: f32,
}

impl Detector {
    pub fn new(sr: f32) -> Self {
        let dec = ((sr / 12000.0).round() as usize).max(1);
        let sr_dec = sr / dec as f32;
        let mut lp1 = Biquad::default();
        let mut lp2 = Biquad::default();
        lp1.set(FilterType::Lowpass, sr, sr_dec * 0.4, 0.54, 0.0);
        lp2.set(FilterType::Lowpass, sr, sr_dec * 0.4, 1.31, 0.0);
        let tau_max = (sr_dec / MIN_HZ) as usize + 2;
        let tau_min = ((sr_dec / MAX_HZ) as usize).max(2);
        let win = (tau_max * 2).next_power_of_two().max(256);
        Self {
            dec,
            lp1,
            lp2,
            phase: 0,
            buf: vec![0.0; win + tau_max + 4],
            w: 0,
            win,
            tau_min,
            tau_max,
            tau_cap: tau_max,
            hop: (sr_dec * 0.004) as usize + 1,
            since: 0,
            d: vec![0.0; tau_max + 2],
            frame: vec![0.0; win + tau_max + 4],
            hz: 0.0,
            confidence: 0.0,
            hint: 0.0,
            rms: 0.0,
            sr_dec,
        }
    }

    /// Latency of an estimate, in device samples (half the analysis window).
    /// Search only between `lo` and `hi` Hz (within MIN_HZ‥MAX_HZ). Latency is unchanged.
    pub fn set_range(&mut self, lo: f32, hi: f32) {
        let (lo, hi) = (lo.clamp(MIN_HZ, MAX_HZ), hi.clamp(MIN_HZ, MAX_HZ));
        self.tau_max = ((self.sr_dec / lo) as usize + 2).min(self.tau_cap);
        self.tau_min = ((self.sr_dec / hi) as usize).max(2).min(self.tau_max.saturating_sub(4));
    }

    pub fn latency(&self) -> usize {
        (self.win + self.tau_cap) / 2 * self.dec
    }

    #[inline]
    pub fn push(&mut self, x: f32) -> bool {
        let y = self.lp2.tick(self.lp1.tick(x));
        self.phase += 1;
        if self.phase < self.dec {
            return false;
        }
        self.phase = 0;
        let n = self.buf.len();
        self.buf[self.w] = y;
        self.w = (self.w + 1) % n;
        self.since += 1;
        if self.since >= self.hop {
            self.since = 0;
            self.analyze();
            return true;
        }
        false
    }

    fn analyze(&mut self) {
        let n = self.buf.len();
        for i in 0..n {
            self.frame[i] = self.buf[(self.w + i) % n];
        }
        let x = &self.frame;
        let (win, tmin, tmax) = (self.win, self.tau_min, self.tau_max);
        let mut e = 0f32;
        for v in &x[..win] {
            e += v * v;
        }
        self.rms = (e / win as f32).sqrt();
        if self.rms < 1e-4 {
            self.hz = 0.0;
            self.confidence = 0.0;
            return;
        }
        // difference function + cumulative mean normalisation
        self.d[0] = 1.0;
        let mut run = 0f32;
        for tau in 1..=tmax {
            let mut s = 0f32;
            for i in 0..win {
                let q = x[i] - x[i + tau];
                s += q * q;
            }
            run += s;
            self.d[tau] = if run > 0.0 { s * tau as f32 / run } else { 1.0 };
        }
        // first dip under the threshold, else the global minimum
        let mut best = 0;
        let mut i = tmin;
        while i < tmax {
            if self.d[i] < 0.15 {
                while i + 1 < tmax && self.d[i + 1] < self.d[i] {
                    i += 1;
                }
                best = i;
                break;
            }
            i += 1;
        }
        if best == 0 {
            best = (tmin..tmax).min_by(|&a, &b| self.d[a].total_cmp(&self.d[b])).unwrap_or(tmin);
            // no clear dip: a loud harmonic can win here; follow the line being sung instead
            if self.hint > 0.0 {
                let th = self.sr_dec / self.hint;
                let (a, b) = (((th * 0.88) as usize).max(tmin), ((th * 1.14) as usize + 1).min(tmax - 1));
                if a < b {
                    let near = (a..=b).min_by(|&x, &y| self.d[x].total_cmp(&self.d[y])).unwrap_or(best);
                    let is_dip = self.d[near] <= self.d[near - 1] && self.d[near] <= self.d[near + 1];
                    if is_dip && self.d[near] < 0.45 {
                        best = near;
                    }
                }
            }
        }
        // Harmonic lock: with a weak fundamental and one loud harmonic (phone mics,
        // low male voices) YIN can stop at T/2 or T/3. Tell them apart by the dip
        // pattern at multiples of the candidate: the true period has dips at all of
        // them, T/3 has none at 2·τ (= 2T/3) but one at 3·τ, T/2 none at 3·τ (odd
        // harmonics present) or a much deeper one at 2·τ (only the 2nd is loud).
        if best > 0 {
            let dip = |k: usize| -> Option<f32> {
                let c = best * k;
                let span = k + 1;
                if c + span >= tmax {
                    return None;
                }
                Some((c - span..=c + span).map(|j| self.d[j]).fold(f32::MAX, f32::min))
            };
            let (l2, l3, l4) = (dip(2), dip(3), dip(4));
            let low = |v: Option<f32>| v.is_some_and(|v| v < 0.25);
            let high = |v: Option<f32>| v.is_some_and(|v| v > 0.4);
            let k = if high(l2) && low(l3) {
                3
            } else if low(l2) && high(l3) && (l4.is_none() || low(l4)) {
                2
            } else if best >= 30 && self.d[best] > 0.03 && l2.is_some_and(|v| v < self.d[best] * 0.5) {
                // loud 2nd harmonic: T/2 and 3T/2 look alike, but the true period is clearly deeper
                2
            } else if high(l2) && high(l3) && low(l4) {
                4
            } else {
                1
            };
            if k > 1 {
                let c = best * k;
                let span = k + 1;
                best = (c - span..=c + span).min_by(|&a, &b| self.d[a].total_cmp(&self.d[b])).unwrap_or(c);
            }
        }
        let dv = self.d[best];
        // parabolic refinement
        let (a, b, c) = (self.d[best - 1], dv, self.d[best + 1]);
        let den = a - 2.0 * b + c;
        let off = if den.abs() > 1e-9 { 0.5 * (a - c) / den } else { 0.0 };
        let tau = best as f32 + off.clamp(-1.0, 1.0);
        self.confidence = (1.0 - dv).clamp(0.0, 1.0);
        self.hz = if dv < 0.35 { self.sr_dec / tau } else { 0.0 };
    }
}

/// TD-PSOLA pitch shifter. Feed one sample, get one sample `latency()` later.
/// `period` is the input's pitch period in samples (0 = unvoiced → transparent),
/// `ratio` the pitch factor, `formant` the formant factor (1 = keep).
pub struct Psola {
    inp: Vec<f32>,
    out: Vec<f32>,
    n: usize,
    tmax: usize,
    next_syn: f64,
    mark: f64,
    hann: Vec<f32>,
}

impl Psola {
    pub fn new(sr: f32) -> Self {
        let tmax = (sr / MIN_HZ * 0.75) as usize + 1; // half-grain bound
        let len = (tmax * 8).next_power_of_two();
        let hann = (0..1025).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / 1024.0).cos()).collect();
        Self { inp: vec![0.0; len], out: vec![0.0; len], n: 0, tmax, next_syn: 0.0, mark: 0.0, hann }
    }

    pub fn latency(&self) -> usize {
        2 * self.tmax
    }

    /// How far back (samples) the grain being shifted right now sits in the input.
    pub fn half_grain(&self) -> usize {
        self.tmax
    }

    pub fn reset(&mut self) {
        self.inp.fill(0.0);
        self.out.fill(0.0);
    }

    #[inline]
    fn win(&self, x: f32) -> f32 {
        // x in 0..1
        let p = x * 1024.0;
        let i = (p as usize).min(1023);
        let f = p - i as f32;
        self.hann[i] + (self.hann[i + 1] - self.hann[i]) * f
    }

    #[inline]
    fn read(&self, t: f64) -> f32 {
        let m = self.inp.len();
        let i = t.floor();
        let f = (t - i) as f32;
        let a = self.inp[(i as i64).rem_euclid(m as i64) as usize];
        let b = self.inp[((i as i64) + 1).rem_euclid(m as i64) as usize];
        a + (b - a) * f
    }

    #[inline]
    pub fn process(&mut self, x: f32, period: f32, ratio: f32, formant: f32) -> f32 {
        let m = self.inp.len();
        let now = self.n;
        self.inp[now % m] = x;
        let voiced = period >= 8.0;
        let t = if voiced { period.min(self.tmax as f32) } else { (self.tmax as f32 * 0.4).max(32.0) };
        let ratio = if voiced { ratio.clamp(0.25, 4.0) } else { 1.0 };
        let fr = if voiced { formant.clamp(0.5, 2.0) } else { 1.0 };
        let lat = self.tmax as f64;
        // synthesis marks up to now
        while self.next_syn <= now as f64 {
            let ts = self.next_syn;
            let c = ts - lat; // input time this grain represents
            // advance analysis marks by the input period
            if !voiced || (self.mark - c).abs() > 4.0 * t as f64 {
                self.mark = c;
            }
            while self.mark + (t as f64) * 0.5 < c {
                self.mark += t as f64;
            }
            let half = t as f64;
            let norm = 1.0 / ratio.max(1e-3);
            let k0 = (ts - half).ceil() as i64;
            let k1 = (ts + half).floor() as i64;
            for k in k0..=k1 {
                let u = ((k as f64 - (ts - half)) / (2.0 * half)) as f32;
                if !(0.0..=1.0).contains(&u) {
                    continue;
                }
                let src = self.mark + (k as f64 - ts) * fr as f64;
                let v = self.read(src) * self.win(u) * norm;
                let idx = (k.rem_euclid(m as i64)) as usize;
                self.out[idx] += v;
            }
            self.next_syn += (t / ratio) as f64;
        }
        // emit the sample all grains are finished with
        let e = now as i64 - self.tmax as i64;
        let y = if e >= 0 {
            let idx = (e as usize) % m;
            let v = self.out[idx];
            self.out[idx] = 0.0;
            v
        } else {
            0.0
        };
        self.n += 1;
        y
    }
}

/// Scale masks (bit k = pitch class k above the key is allowed).
pub fn scale_mask(scale: u32) -> u16 {
    match scale {
        1 => 0b1010_1011_0101, // major: 0 2 4 5 7 9 11
        2 => 0b0101_1010_1101, // natural minor: 0 2 3 5 7 8 10
        3 => 0b0010_1001_0101, // major pentatonic: 0 2 4 7 9
        4 => 0b0100_1010_1001, // minor pentatonic: 0 3 5 7 10
        _ => 0b1111_1111_1111,
    }
}

/// Nearest allowed MIDI note to `m` for `key` (0 = C) and `mask`.
pub fn snap(m: f32, key: u32, mask: u16) -> f32 {
    let base = m.round() as i32;
    let mut best = base as f32;
    let mut bd = f32::MAX;
    for d in -6..=6 {
        let n = base + d;
        let pc = (n - key as i32).rem_euclid(12) as u16;
        if mask & (1 << pc) != 0 {
            let dist = (n as f32 - m).abs();
            if dist < bd {
                bd = dist;
                best = n as f32;
            }
        }
    }
    best
}

// -------------------------------------------------------------- offline analysis

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteBlob {
    /// Seconds into the source.
    pub start: f64,
    pub end: f64,
    /// Average pitch (MIDI, fractional).
    pub pitch: f32,
    /// Peak level 0..1, for drawing the blob's thickness.
    pub level: f32,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PitchAnalysis {
    /// Frames per second of `curve` / `levels`.
    pub rate: f32,
    /// MIDI pitch per frame; NaN where unvoiced (breaths, sibilants, silence).
    pub curve: Vec<f32>,
    pub levels: Vec<f32>,
    pub notes: Vec<NoteBlob>,
}

pub const ANALYSIS_RATE: f32 = 200.0;

/// Analyses a mono signal: pitch curve every 5 ms and note segmentation.
pub fn analyze(mono: &[f32], sr: f32) -> PitchAnalysis {
    let mut det = Detector::new(sr);
    let lat = det.latency();
    let step = (sr / ANALYSIS_RATE) as usize;
    let frames = mono.len() / step + 1;
    let mut curve = vec![f32::NAN; frames];
    let mut levels = vec![0f32; frames];
    let mut cur_hz = 0f32;
    let mut cur_conf = 0f32;
    let mut cur_rms = 0f32;
    let total = mono.len() + lat;
    for i in 0..total {
        let x = if i < mono.len() { mono[i] } else { 0.0 };
        if det.push(x) {
            cur_hz = det.hz;
            cur_conf = det.confidence;
            cur_rms = det.rms;
        }
        if i >= lat && (i - lat) % step == 0 {
            let f = (i - lat) / step;
            if f < frames {
                levels[f] = cur_rms;
                if cur_hz > 0.0 && cur_conf > 0.6 {
                    curve[f] = hz_to_midi(cur_hz);
                }
            }
        }
    }
    // remove octave jumps lasting a few frames and single voiced frames
    for f in 1..frames.saturating_sub(1) {
        let (a, b, c) = (curve[f - 1], curve[f], curve[f + 1]);
        if !b.is_nan() && a.is_nan() && c.is_nan() {
            curve[f] = f32::NAN;
        }
        if !a.is_nan() && !b.is_nan() && !c.is_nan() && (b - a).abs() > 9.0 && (b - c).abs() > 9.0 {
            curve[f] = (a + c) * 0.5;
        }
    }
    let notes = segment(&curve, &levels);
    PitchAnalysis { rate: ANALYSIS_RATE, curve, levels, notes }
}

/// Splits voiced runs into notes where the (smoothed) pitch moves to a new semitone and stays.
fn segment(curve: &[f32], levels: &[f32]) -> Vec<NoteBlob> {
    let dt = 1.0 / ANALYSIS_RATE as f64;
    let mut notes = vec![];
    let mut f = 0;
    while f < curve.len() {
        if curve[f].is_nan() {
            f += 1;
            continue;
        }
        let mut g = f;
        while g < curve.len() && !curve[g].is_nan() {
            g += 1;
        }
        // voiced run f..g; split on sustained semitone changes (≥ 40 ms)
        let mut start = f;
        let mut anchor = curve[f];
        let mut k = f;
        while k < g {
            let hold = 8;
            if (curve[k] - anchor).abs() > 0.75 && k + hold < g && (k..k + hold).all(|j| (curve[j] - anchor).abs() > 0.75 && (curve[j] - curve[k]).abs() < 0.6) {
                push_note(&mut notes, curve, levels, start, k, dt);
                start = k;
                anchor = curve[k];
            }
            // let the anchor follow slow drift
            anchor += (curve[k] - anchor) * 0.02;
            k += 1;
        }
        push_note(&mut notes, curve, levels, start, g, dt);
        f = g;
    }
    notes
}

fn push_note(notes: &mut Vec<NoteBlob>, curve: &[f32], levels: &[f32], a: usize, b: usize, dt: f64) {
    if b <= a + 3 {
        return; // shorter than 20 ms: a glitch, not a note
    }
    let mut v: Vec<f32> = curve[a..b].to_vec();
    v.sort_by(|x, y| x.total_cmp(y));
    let pitch = v[v.len() / 2];
    let level = levels[a..b].iter().fold(0f32, |m, x| m.max(*x));
    notes.push(NoteBlob { start: a as f64 * dt, end: b as f64 * dt, pitch, level });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (std::f32::consts::TAU * hz * i as f32 / sr).sin() * 0.5).collect()
    }

    /// A voice-like tone: harmonics with falling amplitude.
    fn voice(hz: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (1..8).map(|k| (std::f32::consts::TAU * hz * k as f32 * i as f32 / sr).sin() / k as f32).sum::<f32>() * 0.3).collect()
    }

    #[test]
    fn yin_finds_the_pitch() {
        for hz in [82.0, 110.0, 220.0, 440.0, 880.0] {
            let x = voice(hz, 48000.0, 48000);
            let mut d = Detector::new(48000.0);
            for &s in &x {
                d.push(s);
            }
            let err = 1200.0 * (d.hz / hz).log2();
            assert!(err.abs() < 15.0, "{hz} Hz detected as {} ({err:.1} cents)", d.hz);
        }
        let mut d = Detector::new(48000.0);
        let mut seed = 1u32;
        for _ in 0..48000 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            d.push((seed as f32 / u32::MAX as f32 - 0.5) * 0.5);
        }
        assert!(d.hz == 0.0 || d.confidence < 0.6, "noise read as {} Hz", d.hz);
    }

    #[test]
    fn weak_fundamental_with_a_loud_harmonic_is_not_read_as_the_harmonic() {
        // 120 Hz with almost no 1st/2nd harmonic and a huge 3rd (or 2nd) — a phone-mic male voice
        for (amps, name) in [([0.05f32, 0.1, 1.0, 0.3, 0.25, 0.1], "loud 3rd"), ([0.05, 1.0, 0.2, 0.3, 0.1, 0.1], "loud 2nd")] {
            let sr = 48000.0;
            let x: Vec<f32> = (0..24000)
                .map(|i| amps.iter().enumerate().map(|(k, a)| a * (std::f32::consts::TAU * 120.0 * (k + 1) as f32 * i as f32 / sr).sin()).sum::<f32>() * 0.2)
                .collect();
            let mut d = Detector::new(sr);
            let mut wrong = 0;
            for (i, &s) in x.iter().enumerate() {
                if d.push(s) && i > 4800 && (d.hz - 120.0).abs() > 6.0 {
                    wrong += 1;
                }
            }
            assert_eq!(wrong, 0, "{name}: read as {} Hz", d.hz);
        }
    }

    #[test]
    fn psola_shifts_by_the_ratio_and_is_transparent_at_one() {
        let sr = 48000.0;
        for (hz, ratio) in [(220.0f32, 1.0f32), (220.0, 1.12246), (200.0, 0.8909)] {
            let x = voice(hz, sr, 48000);
            let mut p = Psola::new(sr);
            let y: Vec<f32> = x.iter().map(|&s| p.process(s, sr / hz, ratio, 1.0)).collect();
            let mut d = Detector::new(sr);
            for &s in &y[10000..] {
                d.push(s);
            }
            let want = hz * ratio;
            let err = 1200.0 * (d.hz / want).log2();
            assert!(err.abs() < 20.0, "{hz}×{ratio}: got {} Hz, want {want} ({err:.1} cents)", d.hz);
            let rms_in = (x[10000..].iter().map(|v| v * v).sum::<f32>() / 38000.0).sqrt();
            let rms_out = (y[10000..].iter().map(|v| v * v).sum::<f32>() / 38000.0).sqrt();
            assert!((rms_out / rms_in - 1.0).abs() < 0.25, "level changed {rms_in} → {rms_out}");
        }
        let _ = sine;
    }

    #[test]
    fn analysis_segments_a_melody() {
        let sr = 48000.0;
        let mut x = vec![];
        for (hz, secs) in [(220.0, 0.4), (246.94, 0.4), (261.63, 0.4)] {
            x.extend(voice(hz, sr, (sr * secs) as usize));
            x.extend(vec![0.0; (sr * 0.1) as usize]);
        }
        let a = analyze(&x, sr);
        assert_eq!(a.notes.len(), 3, "{:?}", a.notes);
        for (n, want) in a.notes.iter().zip([57.0, 59.0, 60.0]) {
            assert!((n.pitch - want).abs() < 0.2, "{n:?} vs {want}");
        }
    }

    #[test]
    fn snapping_follows_the_scale() {
        let maj = scale_mask(1);
        assert_eq!(snap(60.8, 0, maj), 60.0); // a flat C# → C in C major
        assert_eq!(snap(61.2, 0, maj), 62.0); // a sharp C# → D
        assert_eq!(snap(61.4, 0, scale_mask(0)), 61.0);
    }
}
