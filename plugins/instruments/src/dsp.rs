//! The primitives both instruments are built from.
//!
//! Nothing here allocates, locks or branches on anything but its own state, so
//! every one of these is legal inside [`RtNode::process`](mydaw_rt_core::RtNode).
//! Everything expensive -- an `exp`, a `powf`, a division -- happens in a
//! constructor, which runs on the graph-building thread.
//!
//! # Determinism
//! The only source of randomness in the whole crate is [`Noise`], and it is a
//! plain xorshift seeded by an explicit number. A voice re-seeds it on every
//! trigger, so the same note events produce the same samples *bit for bit* --
//! which is what makes `tests/determinism.rs` a real test rather than a
//! statistical one, and what lets the spectral tests assert numbers.

use std::f32::consts::TAU;

/// Reference pitch: MIDI key 69 is A4.
pub const A4_KEY: f32 = 69.0;
/// Frequency of [`A4_KEY`], in Hz.
pub const A4_HZ: f32 = 440.0;

/// Amplitude below which a decaying voice is considered finished.
///
/// -100 dBFS. Voices are *freed* at this point and their contribution becomes
/// exactly `0.0`, so "the note released" is testable as digital silence rather
/// than as "quiet enough". It also keeps envelope state out of the denormal
/// range, where some CPUs fall off a performance cliff.
pub const SILENCE_THRESHOLD: f32 = 1.0e-5;

/// Equal-tempered frequency of a MIDI key number.
///
/// `f = 440 * 2^((key - 69) / 12)`, exact at key 69 by construction.
pub fn key_to_hz(key: u8) -> f32 {
    A4_HZ * ((f32::from(key) - A4_KEY) / 12.0).exp2()
}

/// A sample rate that can be divided by, substituting a sane default for one
/// that cannot.
///
/// A device reporting 0 Hz (or a NaN) is a bug somewhere else, but the audio
/// thread is the worst possible place to discover it: every derived coefficient
/// would become an infinity and the output would be a NaN storm rather than a
/// wrong note. Falling back keeps the failure audible-but-harmless.
pub fn usable_sample_rate(sample_rate: f64) -> f32 {
    const FALLBACK: f32 = 48_000.0;
    let rate = sample_rate as f32;
    if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        FALLBACK
    }
}

/// Per-sample multiplier for an exponential decay that falls 60 dB in `ms`.
///
/// -60 dB (a factor of 1000) is the convention drum machines and reverbs both
/// use for "decay time", so a `250 ms` constant in this crate means the same
/// thing a reader expects it to mean.
///
/// A degenerate time yields `0.0` -- one sample and gone -- rather than a
/// coefficient of 1.0, which would be an envelope that never ends.
pub fn decay_coefficient(ms: f32, sample_rate: f32) -> f32 {
    let frames = (ms / 1000.0) * sample_rate;
    if frames >= 1.0 {
        0.001_f32.powf(1.0 / frames)
    } else {
        0.0
    }
}

/// Per-sample step for a linear ramp that crosses `range` in `ms`.
///
/// Clamped to `range` so a ramp shorter than one sample completes immediately
/// instead of dividing by zero.
pub fn linear_step(range: f32, ms: f32, sample_rate: f32) -> f32 {
    let frames = (ms / 1000.0) * sample_rate;
    if frames >= 1.0 {
        range / frames
    } else {
        range
    }
}

/// Deterministic white noise: xorshift32.
///
/// # Why not a "real" RNG
/// The requirement is *reproducibility*, not cryptographic quality: three shifts
/// and three xors per sample, no memory traffic, and an identical stream for an
/// identical seed on every machine and every build. A statistical weakness that
/// would matter to a Monte Carlo simulation is inaudible in a hi-hat.
///
/// The state is never allowed to be zero, which is xorshift's one fixed point
/// (it would output silence forever).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Noise {
    state: u32,
}

impl Noise {
    /// Golden-ratio constant, substituted for a zero seed.
    const NONZERO: u32 = 0x9E37_79B9;

    /// A generator with a fixed seed.
    pub const fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { Self::NONZERO } else { seed },
        }
    }

    /// Restart the stream from `seed`. Called on every voice trigger, which is
    /// what makes two identical hits sample-identical.
    pub fn reseed(&mut self, seed: u32) {
        *self = Self::new(seed);
    }

    /// The next 32 raw bits.
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// The next sample, uniform in `-1.0..=1.0`.
    ///
    /// Built from the top 24 bits: every 24-bit integer is exactly representable
    /// in `f32`, so the mapping introduces no rounding of its own and the stream
    /// is reproducible without depending on the compiler's rounding mode.
    pub fn next_bipolar(&mut self) -> f32 {
        const SCALE: f32 = 2.0 / 16_777_215.0; // 2^24 - 1
        (self.next_u32() >> 8) as f32 * SCALE - 1.0
    }
}

/// A one-pole filter, usable as either a lowpass or a highpass.
///
/// `y[n] = y[n-1] + a * (x[n] - y[n-1])` is the lowpass; the highpass is
/// `x[n] - y[n]`, the complementary output of the same state. One multiply-add
/// per sample and one word of state, which is the right size of filter for
/// "roll the bottom off a hi-hat" -- a resonant ladder would be a Phase 2
/// feature with knobs, not a constant buried in a voice.
///
/// 6 dB/octave, so the cutoff constants in this crate are the -3 dB point and
/// not a brick wall. That matters when reading the spectral tests: a 6 kHz
/// highpass still passes a good deal of 3 kHz.
#[derive(Debug, Clone, Copy)]
pub struct OnePole {
    coefficient: f32,
    state: f32,
}

impl OnePole {
    /// A filter with a -3 dB point at `cutoff_hz`.
    ///
    /// The exact impulse-invariant coefficient `1 - e^(-2*pi*fc/fs)` rather than
    /// the `fc/(fc+fs)` approximation: the difference is small at 3 kHz and
    /// large at 6 kHz, and this one costs nothing because it is computed here,
    /// off the audio thread.
    pub fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        let coefficient = if cutoff_hz > 0.0 && sample_rate > 0.0 {
            (1.0 - (-TAU * cutoff_hz / sample_rate).exp()).clamp(0.0, 1.0)
        } else {
            1.0
        };
        Self {
            coefficient,
            state: 0.0,
        }
    }

    /// Clear the filter memory. Called on every trigger so a voice cannot
    /// inherit the tail of the last one.
    pub fn reset(&mut self) {
        self.state = 0.0;
    }

    /// Lowpass output for one sample.
    pub fn low(&mut self, input: f32) -> f32 {
        self.state += self.coefficient * (input - self.state);
        self.state
    }

    /// Highpass output for one sample: the input minus its lowpass.
    pub fn high(&mut self, input: f32) -> f32 {
        input - self.low(input)
    }

    /// The smoothing coefficient. Diagnostics and tests.
    pub fn coefficient(&self) -> f32 {
        self.coefficient
    }
}

/// PolyBLEP correction for a naive sawtooth at normalised phase `phase`.
///
/// A naive saw steps by 2.0 once per cycle, and a step has energy at every
/// frequency -- above Nyquist included, where it folds back down as
/// inharmonic aliasing that no amount of filtering afterwards can remove.
/// PolyBLEP replaces the two samples either side of the discontinuity with a
/// quadratic that approximates the *band-limited* step, which pushes the worst
/// aliasing down by roughly 20-30 dB for one branch and two multiplies.
///
/// `phase` is in `0.0..1.0` and `increment` is the per-sample phase advance
/// (i.e. `frequency / sample_rate`).
pub fn poly_blep(phase: f32, increment: f32) -> f32 {
    if increment <= 0.0 {
        return 0.0;
    }
    if phase < increment {
        // Just after the wrap.
        let t = phase / increment;
        t + t - t * t - 1.0
    } else if phase > 1.0 - increment {
        // Just before it.
        let t = (phase - 1.0) / increment;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// One band-limited sawtooth sample, in `-1.0..=1.0`.
///
/// The naive ramp `2*phase - 1` minus the polyBLEP residual at the wrap.
pub fn poly_blep_saw(phase: f32, increment: f32) -> f32 {
    2.0 * phase - 1.0 - poly_blep(phase, increment)
}

/// One band-limited square sample, in `-1.0..=1.0`.
///
/// Two saws half a cycle apart: `saw(p) - saw(p + 0.5)` is a square, and doing
/// it this way means the *same* polyBLEP that band-limits a saw's one
/// discontinuity band-limits a square's two, with no second correction to get
/// right.
pub fn poly_blep_square(phase: f32, increment: f32) -> f32 {
    let opposite = if phase < 0.5 {
        phase + 0.5
    } else {
        phase - 0.5
    };
    poly_blep_saw(phase, increment) - poly_blep_saw(opposite, increment)
}

/// Waveform indices for [`analog_wave`]. `0`/`1` stay saw/square so old
/// `wave` values in project files keep their sound.
pub const WAVE_SAW: usize = 0;
pub const WAVE_SQUARE: usize = 1;
pub const WAVE_SINE: usize = 2;
pub const WAVE_TRIANGLE: usize = 3;
pub const WAVE_NOISE: usize = 4;
pub const WAVE_COUNT: u32 = 5;

/// Frequency ratio of one oscillator: `2^(coarse/12 + fine/1200)`.
pub fn pitch_ratio(coarse_semitones: f32, fine_cents: f32) -> f32 {
    let coarse = if coarse_semitones.is_finite() {
        coarse_semitones.clamp(-24.0, 24.0)
    } else {
        0.0
    };
    let fine = if fine_cents.is_finite() {
        fine_cents.clamp(-100.0, 100.0)
    } else {
        0.0
    };
    2f32.powf(coarse / 12.0 + fine / 1_200.0)
}

/// One sample of a classic analog shape. Saw and square are polyBLEP; sine
/// has no aliases to correct; triangle's harmonics fall fast enough that a
/// naive fold is honest at musical pitches; noise is the voice's xorshift.
pub fn analog_wave(wave: usize, phase: f32, increment: f32, noise: &mut Noise) -> f32 {
    match wave {
        WAVE_SQUARE => poly_blep_square(phase, increment),
        WAVE_SINE => (phase * TAU).sin(),
        WAVE_TRIANGLE => {
            if phase < 0.5 {
                4.0 * phase - 1.0
            } else {
                3.0 - 4.0 * phase
            }
        }
        WAVE_NOISE => noise.next_bipolar(),
        _ => poly_blep_saw(phase, increment),
    }
}

/// A two-pole state-variable filter, topology-preserving.
///
/// The one-pole this replaced could not resonate, and a lowpass without
/// resonance is a tone control rather than an instrument's filter: everything
/// anyone reaches a cutoff knob *for* -- the vocal quality at the corner, the
/// squelch of a fast envelope -- lives in the peak. Zavalishin's TPT form, which
/// stays stable when the cutoff is swept every few samples (which is exactly
/// what an envelope does to it).
#[derive(Debug, Clone, Copy, Default)]
pub struct Svf {
    a1: f32,
    a2: f32,
    a3: f32,
    ic1: f32,
    ic2: f32,
}

impl Svf {
    /// A filter at `cutoff_hz` with quality `q`, with no state.
    pub fn new(cutoff_hz: f32, q: f32, sample_rate: f32) -> Self {
        let mut filter = Self::default();
        filter.set(cutoff_hz, q, sample_rate);
        filter
    }

    /// Re-aim the filter. Cheap enough to call at control rate, which is what
    /// makes a filter envelope possible; `tan` is the only real cost.
    pub fn set(&mut self, cutoff_hz: f32, q: f32, sample_rate: f32) {
        // Held below a fifth of the rate: `tan` goes to infinity at Nyquist and
        // a cutoff a knob can reach must not be able to produce one.
        let nyquist_fifth = sample_rate * 0.2;
        let cutoff = cutoff_hz.clamp(10.0, nyquist_fifth.max(20.0));
        let g = (std::f32::consts::PI * cutoff / sample_rate).tan();
        let k = 1.0 / q.clamp(0.4, 20.0);
        self.a1 = 1.0 / (1.0 + g * (g + k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    /// Forget the state. For a voice being restarted from silence.
    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }

    /// One lowpass sample.
    pub fn low(&mut self, input: f32) -> f32 {
        let v3 = input - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        v2
    }
}

/// Advance a normalised phase, wrapping into `0.0..1.0`.
///
/// Subtraction rather than `%`: the increment is always well below 1.0 for any
/// audible frequency, so one conditional subtract is exact and avoids the slow
/// path of a floating-point remainder in the inner loop.
pub fn advance_phase(phase: f32, increment: f32) -> f32 {
    let next = phase + increment;
    if next >= 1.0 {
        next - 1.0
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    #[test]
    fn equal_temperament_anchors_on_a440() {
        assert!((key_to_hz(69) - 440.0).abs() < 1e-3);
        assert!((key_to_hz(81) - 880.0).abs() < 1e-2, "an octave up");
        assert!((key_to_hz(57) - 220.0).abs() < 1e-2, "an octave down");
        assert!((key_to_hz(60) - 261.626).abs() < 0.01, "middle C");
        // The top of a 128-key keyboard, where anti-aliasing has to work.
        assert!((key_to_hz(108) - 4186.01).abs() < 0.1);
    }

    #[test]
    fn a_degenerate_sample_rate_is_replaced_rather_than_propagated() {
        assert_eq!(usable_sample_rate(44_100.0), 44_100.0);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let rate = usable_sample_rate(bad);
            assert!(rate.is_finite() && rate > 0.0, "{bad} produced {rate}");
        }
    }

    #[test]
    fn a_decay_coefficient_reaches_minus_sixty_db_at_the_advertised_time() {
        let coefficient = decay_coefficient(250.0, SR);
        let frames = (0.250 * SR) as usize;
        let mut level = 1.0_f32;
        for _ in 0..frames {
            level *= coefficient;
        }
        assert!(
            (level - 0.001).abs() < 1e-4,
            "250 ms should land on -60 dB, got {level}"
        );
        // And a ramp shorter than a sample ends immediately instead of never.
        assert_eq!(decay_coefficient(0.0, SR), 0.0);
    }

    #[test]
    fn a_linear_step_crosses_its_range_in_the_advertised_time() {
        let step = linear_step(1.0, 5.0, SR);
        assert!(
            (1.0 / step - 240.0).abs() < 1e-3,
            "5 ms at 48 kHz is 240 frames"
        );
        assert_eq!(linear_step(0.4, 0.0, SR), 0.4, "an instant ramp completes");
    }

    #[test]
    fn the_noise_stream_is_reproducible_and_bounded() {
        let mut a = Noise::new(0x1234_5678);
        let mut b = Noise::new(0x1234_5678);
        let mut extremes = (1.0_f32, -1.0_f32);
        for _ in 0..100_000 {
            let (x, y) = (a.next_bipolar(), b.next_bipolar());
            assert_eq!(x.to_bits(), y.to_bits(), "same seed, same bits");
            assert!((-1.0..=1.0).contains(&x), "{x} escaped the range");
            extremes.0 = extremes.0.min(x);
            extremes.1 = extremes.1.max(x);
        }
        // It really does cover the range, so a voice using it is broadband.
        assert!(extremes.0 < -0.99 && extremes.1 > 0.99, "{extremes:?}");
    }

    #[test]
    fn the_noise_stream_never_collapses_to_zero() {
        // Zero is xorshift's one fixed point: seeded with it, a naive
        // implementation outputs silence forever.
        let mut noise = Noise::new(0);
        assert_ne!(noise.next_u32(), 0);
        let mut previous = 0;
        for _ in 0..1_000 {
            let next = noise.next_u32();
            assert_ne!(next, 0);
            assert_ne!(next, previous, "the stream must not stick");
            previous = next;
        }
    }

    #[test]
    fn reseeding_restarts_the_same_stream() {
        let mut noise = Noise::new(7);
        let first: Vec<f32> = (0..64).map(|_| noise.next_bipolar()).collect();
        noise.reseed(7);
        let second: Vec<f32> = (0..64).map(|_| noise.next_bipolar()).collect();
        assert_eq!(first, second);
    }

    #[test]
    fn the_one_pole_splits_a_signal_into_complementary_halves() {
        // DC goes entirely to the lowpass; the highpass rejects it.
        let mut filter = OnePole::new(1_000.0, SR);
        let mut low = 0.0;
        for _ in 0..10_000 {
            low = filter.low(1.0);
        }
        assert!(
            (low - 1.0).abs() < 1e-3,
            "DC should pass the lowpass, got {low}"
        );

        let mut filter = OnePole::new(1_000.0, SR);
        let mut high = 1.0;
        for _ in 0..10_000 {
            high = filter.high(1.0);
        }
        assert!(
            high.abs() < 1e-3,
            "DC must not pass the highpass, got {high}"
        );
    }

    #[test]
    fn the_one_pole_cutoff_is_minus_three_db_at_the_advertised_frequency() {
        // Drive it with a sine at the cutoff and measure the steady-state peak.
        const CUTOFF: f32 = 3_000.0;
        let mut filter = OnePole::new(CUTOFF, SR);
        let increment = CUTOFF / SR;
        let mut phase = 0.0_f32;
        let mut peak = 0.0_f32;
        for i in 0..4_000 {
            let output = filter.low((phase * TAU).sin());
            if i > 2_000 {
                peak = peak.max(output.abs());
            }
            phase = advance_phase(phase, increment);
        }
        let db = 20.0 * peak.log10();
        assert!(
            (db + 3.01).abs() < 0.3,
            "a one-pole at its cutoff should be -3 dB, measured {db:.2} dB"
        );
    }

    #[test]
    fn the_one_pole_survives_a_nonsense_configuration() {
        let mut filter = OnePole::new(0.0, SR);
        assert_eq!(filter.coefficient(), 1.0, "degenerate = pass-through");
        assert_eq!(filter.low(0.5), 0.5);
        filter.reset();
        assert_eq!(filter.low(0.0), 0.0);
    }

    #[test]
    fn the_naive_saw_and_the_blep_saw_agree_away_from_the_discontinuity() {
        let increment = 440.0 / SR;
        for step in 1..100 {
            let phase = step as f32 / 100.0;
            if phase < increment || phase > 1.0 - increment {
                continue;
            }
            assert_eq!(poly_blep(phase, increment), 0.0);
            assert_eq!(poly_blep_saw(phase, increment), 2.0 * phase - 1.0);
        }
    }

    #[test]
    fn analog_wave_keeps_saw_and_square_and_adds_sine_triangle_noise() {
        let increment = 440.0 / SR;
        let mut noise = Noise::new(1);
        assert_eq!(
            analog_wave(WAVE_SAW, 0.3, increment, &mut noise),
            poly_blep_saw(0.3, increment)
        );
        assert_eq!(
            analog_wave(WAVE_SQUARE, 0.3, increment, &mut noise),
            poly_blep_square(0.3, increment)
        );
        assert!((analog_wave(WAVE_SINE, 0.0, increment, &mut noise)).abs() < 1e-6);
        assert!((analog_wave(WAVE_SINE, 0.25, increment, &mut noise) - 1.0).abs() < 1e-5);
        assert!((analog_wave(WAVE_TRIANGLE, 0.0, increment, &mut noise) + 1.0).abs() < 1e-6);
        assert!((analog_wave(WAVE_TRIANGLE, 0.25, increment, &mut noise)).abs() < 1e-6);
        let a = analog_wave(WAVE_NOISE, 0.0, increment, &mut Noise::new(3));
        let b = analog_wave(WAVE_NOISE, 0.0, increment, &mut Noise::new(3));
        assert_eq!(a, b);
        assert!(a.abs() <= 1.0);
    }

    #[test]
    fn pitch_ratio_is_an_octave_at_plus_twelve_and_unity_at_zero() {
        assert!((pitch_ratio(0.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((pitch_ratio(12.0, 0.0) - 2.0).abs() < 1e-5);
        assert!((pitch_ratio(-12.0, 0.0) - 0.5).abs() < 1e-5);
        assert!(pitch_ratio(0.0, 100.0) > 1.0);
    }

    /// The whole point of polyBLEP, measured rather than asserted: a high saw
    /// rendered naively folds energy back below its own fundamental, and the
    /// corrected one folds far less.
    #[test]
    fn poly_blep_suppresses_the_aliasing_a_naive_saw_produces() {
        let increment = 4_186.01 / SR; // key 108, the top of the keyboard
        let mut phase = 0.0_f32;
        let (mut naive, mut blep) = (Vec::new(), Vec::new());
        for _ in 0..8_192 {
            naive.push(2.0 * phase - 1.0);
            blep.push(poly_blep_saw(phase, increment));
            phase = advance_phase(phase, increment);
        }

        // Energy below the fundamental can only be aliasing: a saw has no
        // partials there at all.
        let naive_below = energy_below(&naive, 4_000.0, SR);
        let blep_below = energy_below(&blep, 4_000.0, SR);
        assert!(
            blep_below < naive_below * 0.5,
            "polyBLEP left {blep_below:.3e} of sub-fundamental energy against \
             the naive saw's {naive_below:.3e}"
        );
    }

    /// Sum of squared magnitudes below `limit_hz`, by direct evaluation of the
    /// Goertzel-style inner products. Small, exact, and no extra dependency.
    fn energy_below(samples: &[f32], limit_hz: f32, sample_rate: f32) -> f64 {
        let bins = 64;
        let mut total = 0.0_f64;
        for bin in 1..=bins {
            let hz = f64::from(limit_hz) * f64::from(bin) / f64::from(bins);
            let omega = std::f64::consts::TAU * hz / f64::from(sample_rate);
            let (mut re, mut im) = (0.0_f64, 0.0_f64);
            for (n, sample) in samples.iter().enumerate() {
                let angle = omega * n as f64;
                re += f64::from(*sample) * angle.cos();
                im -= f64::from(*sample) * angle.sin();
            }
            total += (re * re + im * im) / (samples.len() as f64).powi(2);
        }
        total
    }
}
