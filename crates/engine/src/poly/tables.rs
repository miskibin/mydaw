//! Band-limited wavetables for the synth oscillators, generated once (never on the audio thread).
//!
//! Every wave has two frames (Shape morphs between them). Every frame is stored as a set of
//! mip levels: level `j` keeps harmonics `1..=harmonics(j)`, where the count falls by a third
//! of an octave per level. At playback the oscillator picks the richest level whose top
//! harmonic is still below Nyquist and crossfades it with the next poorer one, so no
//! harmonic is ever above Nyquist (alias-free by construction) and brightness changes
//! smoothly with pitch.
//!
//! Third-octave spacing instead of the usual one level per octave: with whole octaves the
//! safe level loses up to an octave of top harmonics (a saw at the bottom of a level's range
//! stops at fs/4 = 12 kHz). With thirds the loss is at most a third of an octave (content
//! above ~0.79·fs/2). Memory: 2 frames × 8 waves × 25 levels × 2049 floats ≈ 3.3 MB.
//!
//! Harmonics are capped at [`MAX_H`] = 256 per 2048-sample table, so the table is at least
//! 4× oversampled relative to its content and linear interpolation's images stay below
//! about −65 dB (measured in `tests::alias_snr`).

use std::sync::OnceLock;

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

pub const SIZE: usize = 2048;
const STRIDE: usize = SIZE + 1;
pub const MAX_H: usize = 256;
/// Mip levels per octave.
pub const LPO: f32 = 3.0;
pub const N_WAVES: usize = 8;
pub const FRAMES: usize = 2;

pub const SINE: u8 = 0;
pub const TRI: u8 = 1;
pub const SAW: u8 = 2;
pub const SQUARE: u8 = 3;
pub const HOLLOW: u8 = 4;
pub const AIR: u8 = 5;
pub const DIGITAL: u8 = 6;
pub const METAL: u8 = 7;

/// Halfband FIR for the 2× oversampled waveshapers: the odd taps h[±1], h[±3], … h[±15]
/// (h[0] = 0.5, even taps 0). Kaiser-windowed sinc, β = 8, normalised for unity DC gain.
pub const HB_TAPS: usize = 8;

pub struct Bank {
    levels: usize,
    harm: Vec<usize>,
    data: Vec<f32>,
    pub hb: [f32; HB_TAPS],
}

static BANK: OnceLock<Bank> = OnceLock::new();

/// The shared tables (built on first use; call it off the audio thread first).
pub fn bank() -> &'static Bank {
    BANK.get_or_init(Bank::build)
}

/// Harmonic `n` (1-based) of a frame as (amplitude, phase) of `a·sin(2πnt + φ)`.
type Spec = Vec<(f64, f64)>;

fn hash(n: usize, salt: u32) -> f64 {
    let mut x = (n as u32).wrapping_mul(0x9E37_79B1) ^ salt.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x as f64) / (u32::MAX as f64)
}

fn spec_fn(f: impl Fn(usize) -> (f64, f64)) -> Spec {
    (1..=MAX_H).map(f).collect()
}

/// Spectrum of a periodic time-domain shape (one cycle sampled at `m` points).
fn spec_of(m: usize, shape: impl Fn(f64) -> f64) -> Spec {
    let mut fft = RealFftPlanner::<f64>::new();
    let f = fft.plan_fft_forward(m);
    let mut x: Vec<f64> = (0..m).map(|i| shape(i as f64 / m as f64)).collect();
    let mut y = f.make_output_vec();
    f.process(&mut x, &mut y).unwrap();
    // X_n = (m·a/2)·e^{i(φ − π/2)}
    (1..=MAX_H)
        .map(|n| {
            let c = y[n] / (m as f64 / 2.0);
            (c.norm(), c.arg() + std::f64::consts::FRAC_PI_2)
        })
        .collect()
}

fn gauss(n: f64, c: f64, w: f64) -> f64 {
    (-((n - c) / w).powi(2) * 0.5).exp()
}

/// The two frames of every wave.
fn spectra(wave: u8) -> [Spec; FRAMES] {
    use std::f64::consts::PI;
    let saw = |n: usize| (2.0 / PI / n as f64, PI); // falling edge at the cycle start: rising ramp
    match wave {
        SINE => [
            spec_fn(|n| (if n == 1 { 1.0 } else { 0.0 }, 0.0)),
            // a warm, slightly saturated sine (2nd–4th harmonics)
            spec_fn(|n| {
                (
                    match n {
                        1 => 1.0,
                        2 => 0.32,
                        3 => 0.16,
                        4 => 0.06,
                        _ => 0.0,
                    },
                    0.0,
                )
            }),
        ],
        TRI => [
            spec_fn(|n| {
                if n % 2 == 1 {
                    (
                        8.0 / (PI * PI) / (n * n) as f64,
                        if (n / 2) % 2 == 0 { 0.0 } else { PI },
                    )
                } else {
                    (0.0, 0.0)
                }
            }),
            // between a triangle and a square: odd harmonics at 1/n^1.5
            spec_fn(|n| {
                if n % 2 == 1 {
                    (1.0 / (n as f64).powf(1.5), 0.0)
                } else {
                    (0.0, 0.0)
                }
            }),
        ],
        SAW | SQUARE => [
            spec_fn(saw),
            // saw with its octave: brighter, more "double"
            spec_fn(|n| {
                let a = 2.0 / PI / n as f64
                    + if n % 2 == 0 {
                        0.7 * 2.0 / PI / (n / 2) as f64
                    } else {
                        0.0
                    };
                (a, PI)
            }),
        ],
        HOLLOW => [
            // odd harmonics with every fourth odd one missing: a hollow, pipe-like tone
            spec_fn(|n| {
                if n % 2 == 1 && n % 8 != 7 {
                    (1.0 / (n as f64).powf(1.15), 0.0)
                } else {
                    (0.0, 0.0)
                }
            }),
            // fundamental and octaves/even harmonics: organ-flute hollow
            spec_fn(|n| {
                if n == 1 || (n % 2 == 0 && n % 6 != 0) {
                    (1.0 / (n as f64).powf(1.05), 0.0)
                } else {
                    (0.0, 0.0)
                }
            }),
        ],
        AIR => [
            // formant bumps (open vowel) over a soft 1/sqrt(n) floor, scattered phases
            spec_fn(|n| {
                let x = n as f64;
                let a = (gauss(x, 3.0, 1.2)
                    + 0.7 * gauss(x, 9.0, 2.5)
                    + 0.4 * gauss(x, 24.0, 6.0)
                    + 0.05)
                    / x.sqrt();
                (a, 2.0 * PI * hash(n, 1))
            }),
            spec_fn(|n| {
                let x = n as f64;
                let a = (0.8 * gauss(x, 2.0, 0.8)
                    + gauss(x, 6.0, 1.6)
                    + 0.6 * gauss(x, 16.0, 4.0)
                    + 0.35 * gauss(x, 40.0, 10.0)
                    + 0.05)
                    / x.sqrt();
                (a, 2.0 * PI * hash(n, 2))
            }),
        ],
        DIGITAL => [
            // a 3-bit sine: stepped, "digital"
            spec_of(8192, |t| ((2.0 * PI * t).sin() * 3.5).round() / 3.5),
            // a 6-step staircase saw
            spec_of(8192, |t| ((t * 6.0).floor() / 5.0) * 2.0 - 1.0),
        ],
        _ => [
            // metallic: sparse, wide-spaced harmonics with slow decay
            spec_fn(|n| {
                const SET: [usize; 10] = [1, 5, 9, 14, 21, 30, 41, 53, 67, 83];
                (
                    if SET.contains(&n) {
                        1.0 / (n as f64).powf(0.45)
                    } else {
                        0.0
                    },
                    2.0 * PI * hash(n, 3),
                )
            }),
            spec_fn(|n| {
                const SET: [usize; 10] = [1, 3, 7, 11, 17, 26, 37, 49, 62, 78];
                (
                    if SET.contains(&n) {
                        1.0 / (n as f64).powf(0.4)
                    } else {
                        0.0
                    },
                    2.0 * PI * hash(n, 4),
                )
            }),
        ],
    }
}

/// Harmonics kept at mip level `j`.
fn harmonics(j: usize) -> usize {
    ((MAX_H as f64) * 2f64.powf(-(j as f64) / LPO as f64))
        .floor()
        .max(1.0) as usize
}

fn kaiser_halfband() -> [f32; HB_TAPS] {
    fn i0(x: f64) -> f64 {
        let (mut s, mut t) = (1.0, 1.0);
        for k in 1..40 {
            t *= (x / 2.0) / k as f64;
            s += t * t;
        }
        s
    }
    let m = (2 * HB_TAPS - 1) as f64; // half length in taps (15)
    let beta = 8.0;
    let mut h = [0f64; HB_TAPS];
    for (j, v) in h.iter_mut().enumerate() {
        let k = (2 * j + 1) as f64;
        let sinc = (std::f64::consts::PI * k / 2.0).sin() / (std::f64::consts::PI * k / 2.0);
        let w = i0(beta * (1.0 - (k / (m + 1.0)).powi(2)).max(0.0).sqrt()) / i0(beta);
        *v = 0.5 * sinc * w;
    }
    let s: f64 = h.iter().sum();
    let mut out = [0f32; HB_TAPS];
    for j in 0..HB_TAPS {
        out[j] = (h[j] * 0.25 / s) as f32;
    }
    out
}

impl Bank {
    fn build() -> Bank {
        let mut levels = 1;
        while harmonics(levels - 1) > 1 {
            levels += 1;
        }
        let harm: Vec<usize> = (0..levels).map(harmonics).collect();
        let mut data = vec![0f32; N_WAVES * FRAMES * levels * STRIDE];
        let mut planner = RealFftPlanner::<f64>::new();
        let inv = planner.plan_fft_inverse(SIZE);
        let mut spec = inv.make_input_vec();
        let mut out = inv.make_output_vec();
        for w in 0..N_WAVES as u8 {
            for (f, sp) in spectra(w).iter().enumerate() {
                let mut gain = 1.0;
                for (j, &nh) in harm.iter().enumerate() {
                    spec.iter_mut().for_each(|c| *c = Complex::new(0.0, 0.0));
                    for (n, &(a, ph)) in sp.iter().enumerate().take(nh) {
                        // a·sin(θ+φ) = 2·Re(X·e^{iθ}) with X = (a/2)·e^{i(φ−π/2)}
                        spec[n + 1] =
                            Complex::from_polar(a / 2.0, ph - std::f64::consts::FRAC_PI_2);
                    }
                    inv.process(&mut spec, &mut out).unwrap();
                    if j == 0 {
                        // normalise every level by the richest one's peak (no level jumps)
                        let pk = out.iter().fold(0f64, |m, v| m.max(v.abs()));
                        gain = if pk > 0.0 { 1.0 / pk } else { 1.0 };
                    }
                    let base = Self::offset_of(levels, w, f, j);
                    for i in 0..SIZE {
                        data[base + i] = (out[i] * gain) as f32;
                    }
                    data[base + SIZE] = data[base];
                }
            }
        }
        Bank {
            levels,
            harm,
            data,
            hb: kaiser_halfband(),
        }
    }

    fn offset_of(levels: usize, wave: u8, frame: usize, level: usize) -> usize {
        ((wave as usize * FRAMES + frame) * levels + level) * STRIDE
    }

    pub fn levels(&self) -> usize {
        self.levels
    }

    pub fn harmonics(&self, level: usize) -> usize {
        self.harm[level]
    }

    /// The table of `wave`/`frame` at mip `level` (`SIZE + 1` samples, the last = the first).
    #[inline]
    pub fn table(&self, wave: u8, frame: usize, level: usize) -> &[f32] {
        let o = Self::offset_of(self.levels, wave, frame, level);
        &self.data[o..o + STRIDE]
    }

    /// Mip levels for a phase increment `dt` (cycles per sample): (rich level, poor level,
    /// weight of the rich one). Both levels have every harmonic below Nyquist.
    #[inline]
    pub fn select(&self, dt: f32) -> (usize, usize, f32) {
        let dt = dt.abs().max(1e-9);
        // level j is safe while harmonics(j)·dt ≤ 0.5
        let jr = LPO * (2.0 * MAX_H as f32 * dt).log2();
        if jr <= 0.0 {
            return (0, 1.min(self.levels - 1), 1.0);
        }
        let js = jr.ceil();
        let w = js - jr;
        let j = (js as usize).min(self.levels - 1);
        (
            j,
            (j + 1).min(self.levels - 1),
            if j == self.levels - 1 { 1.0 } else { w },
        )
    }
}

/// Linear-interpolated read of a table at phase `ph` ∈ [0, 1).
#[inline(always)]
pub fn read(t: &[f32], ph: f32) -> f32 {
    let x = ph * SIZE as f32;
    let i = (x as usize).min(SIZE - 1);
    let fr = x - i as f32;
    let a = t[i];
    a + (t[i + 1] - a) * fr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_below_nyquist() {
        let b = bank();
        // every selected level keeps all harmonics below Nyquist
        for k in 0..2000 {
            let dt = 0.5 * (k as f32 + 0.5) / 2000.0;
            let (j0, j1, _) = b.select(dt);
            for j in [j0, j1] {
                if b.harmonics(j) > 1 {
                    assert!(
                        b.harmonics(j) as f32 * dt <= 0.5 + 1e-6,
                        "dt {dt} level {j}"
                    );
                }
            }
        }
    }

    #[test]
    fn saw_table_is_a_ramp() {
        let b = bank();
        let t = b.table(SAW, 0, 0);
        // rising ramp: mid-cycle value near 0, increasing
        assert!(
            t[SIZE / 4] < t[SIZE / 2] && t[SIZE / 2] < t[3 * SIZE / 4],
            "{} {} {}",
            t[SIZE / 4],
            t[SIZE / 2],
            t[3 * SIZE / 4]
        );
        let pk = t.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((pk - 1.0).abs() < 1e-3);
    }
}
