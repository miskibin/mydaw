//! Second-order sections: the RBJ cookbook, in `f64`, plus the magnitude
//! response that draws them.
//!
//! # Why `f64` state and not `f32`
//! A 20 Hz highpass at 48 kHz has poles at a radius of about 0.9974. In `f32`
//! the coefficient quantisation around that radius is comparable to the distance
//! from the pole to the unit circle, which shows up as a filter that is a decibel
//! off where it was asked to be and, at high Q, as one that is not quite stable.
//! Transposed direct form II in `f64` costs five multiplies and four adds per
//! sample -- ten biquads at 48 kHz is under half a megaflop per second -- and the
//! problem simply does not arise. The buffers stay `f32`; only the recursion is
//! `f64`.
//!
//! # Why transposed direct form II
//! Of the four direct forms it has the best round-off behaviour with a single
//! accumulator per state word, and its two state words *are* the filter's
//! memory, so [`Biquad::reset`] is two stores rather than a bookkeeping exercise.
//!
//! # Denormals
//! The audio thread runs with FTZ/DAZ set (see
//! [`disable_denormals`](mydaw_rt_core::disable_denormals)), which handles the
//! `f32` buffers. FTZ does *not* cover `f64` arithmetic on every path, so the
//! state is additionally flushed by hand once it falls below
//! [`DENORMAL_FLOOR`] -- one compare per sample, and a filter that has been fed
//! silence for a second is genuinely at zero rather than at 1e-320.

use std::f64::consts::TAU;

/// Below this, a state word is set to exactly zero.
///
/// -300 dBFS: far below anything audible, far above the subnormal range, and
/// small enough that flushing it can never truncate a decaying tail that
/// somebody could hear.
pub const DENORMAL_FLOOR: f64 = 1.0e-15;

/// The five normalised coefficients of a second-order section.
///
/// Already divided through by `a0`, so [`Biquad::process`] never divides.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadCoeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Default for BiquadCoeffs {
    /// Unity: `y = x`.
    fn default() -> Self {
        Self::BYPASS
    }
}

impl BiquadCoeffs {
    /// A section that passes its input through unchanged.
    pub const BYPASS: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Divide through by `a0`, refusing a degenerate section.
    fn normalise(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        if !a0.is_finite() || a0.abs() < f64::EPSILON {
            return Self::BYPASS;
        }
        let out = Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        };
        if out.b0.is_finite()
            && out.b1.is_finite()
            && out.b2.is_finite()
            && out.a1.is_finite()
            && out.a2.is_finite()
        {
            out
        } else {
            Self::BYPASS
        }
    }

    /// `|H(e^{jω})|` at `hz`, as a linear magnitude.
    ///
    /// Evaluated from the coefficients rather than by running an impulse
    /// through the filter, so the EQ's curve display and its tests are reading
    /// the same numbers the audio path is using.
    ///
    /// The closed form is the ratio of two second-order cosine polynomials:
    /// `|H|² = (b0² + b1² + b2² + 2(b0b1 + b1b2)cos ω + 2b0b2 cos 2ω) /
    ///         (1 + a1² + a2² + 2(a1 + a1a2)cos ω + 2a2 cos 2ω)`.
    pub fn magnitude_at(&self, hz: f64, sample_rate: f64) -> f64 {
        if !sample_rate.is_finite() || sample_rate <= 0.0 || !hz.is_finite() {
            return 1.0;
        }
        let omega = TAU * hz / sample_rate;
        let (cos1, cos2) = (omega.cos(), (2.0 * omega).cos());
        let num = self.b0 * self.b0
            + self.b1 * self.b1
            + self.b2 * self.b2
            + 2.0 * (self.b0 * self.b1 + self.b1 * self.b2) * cos1
            + 2.0 * self.b0 * self.b2 * cos2;
        let den = 1.0
            + self.a1 * self.a1
            + self.a2 * self.a2
            + 2.0 * (self.a1 + self.a1 * self.a2) * cos1
            + 2.0 * self.a2 * cos2;
        if den <= 0.0 || !den.is_finite() || num < 0.0 || !num.is_finite() {
            return 1.0;
        }
        (num / den).sqrt()
    }
}

/// Shared intermediates every RBJ section needs.
struct Rbj {
    cos_w: f64,
    alpha: f64,
}

impl Rbj {
    /// `ω₀`, `cos ω₀` and `α = sin ω₀ / 2Q` for a cutoff and a Q.
    ///
    /// The frequency is clamped just under Nyquist: a cutoff *at* Nyquist makes
    /// `sin ω₀` zero and the section degenerate, and a frequency knob dragged to
    /// the top of its range on a 44.1 kHz device would otherwise do exactly that.
    fn new(hz: f64, q: f64, sample_rate: f64) -> Self {
        // A device reporting a rate of zero is a bug somewhere else, and the
        // audio thread is the worst place to meet it: `clamp` panics when its
        // bounds cross, so the guard has to come first.
        let sample_rate = if sample_rate.is_finite() && sample_rate > 1.0 {
            sample_rate
        } else {
            crate::dsp::FALLBACK_SAMPLE_RATE
        };
        let nyquist = sample_rate * 0.5;
        let hz = if hz.is_finite() { hz } else { nyquist * 0.5 };
        let hz = hz.clamp(1.0, nyquist * 0.999);
        let q = if q.is_finite() && q > 1.0e-4 {
            q
        } else {
            1.0e-4
        };
        let omega = TAU * hz / sample_rate;
        let sin_w = omega.sin();
        Self {
            cos_w: omega.cos(),
            alpha: sin_w / (2.0 * q),
        }
    }
}

/// A 12 dB/octave Butterworth-family highpass at `hz` with resonance `q`.
///
/// `q = 1/√2` is the maximally flat (Butterworth) response; the EQ's 24 dB/oct
/// slope is two of these in series at the staggered Qs Butterworth asks for.
pub fn highpass(hz: f64, q: f64, sample_rate: f64) -> BiquadCoeffs {
    let Rbj { cos_w, alpha } = Rbj::new(hz, q, sample_rate);
    let b0 = (1.0 + cos_w) / 2.0;
    BiquadCoeffs::normalise(
        b0,
        -(1.0 + cos_w),
        b0,
        1.0 + alpha,
        -2.0 * cos_w,
        1.0 - alpha,
    )
}

/// A 12 dB/octave lowpass at `hz` with resonance `q`.
pub fn lowpass(hz: f64, q: f64, sample_rate: f64) -> BiquadCoeffs {
    let Rbj { cos_w, alpha } = Rbj::new(hz, q, sample_rate);
    let b0 = (1.0 - cos_w) / 2.0;
    BiquadCoeffs::normalise(b0, 1.0 - cos_w, b0, 1.0 + alpha, -2.0 * cos_w, 1.0 - alpha)
}

/// A peaking (bell) section: `gain_db` at `hz`, width set by `q`.
///
/// Q here is the RBJ definition -- the ratio of centre frequency to the
/// bandwidth measured between the points *halfway in decibels* to the peak --
/// which is what every parametric EQ's Q knob means.
pub fn peaking(hz: f64, q: f64, gain_db: f64, sample_rate: f64) -> BiquadCoeffs {
    let Rbj { cos_w, alpha } = Rbj::new(hz, q, sample_rate);
    let a = 10.0_f64.powf(gain_db / 40.0);
    BiquadCoeffs::normalise(
        1.0 + alpha * a,
        -2.0 * cos_w,
        1.0 - alpha * a,
        1.0 + alpha / a,
        -2.0 * cos_w,
        1.0 - alpha / a,
    )
}

/// A low shelf: `gain_db` below `hz`, unity above, slope set by `q`.
///
/// `q = 1/√2` is the shelf with no overshoot at the corner, which is what a
/// shelf knob at its default should do.
pub fn low_shelf(hz: f64, q: f64, gain_db: f64, sample_rate: f64) -> BiquadCoeffs {
    let Rbj { cos_w, alpha } = Rbj::new(hz, q, sample_rate);
    let a = 10.0_f64.powf(gain_db / 40.0);
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
    BiquadCoeffs::normalise(
        a * ((a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha),
        2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w),
        a * ((a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha),
        (a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha,
        -2.0 * ((a - 1.0) + (a + 1.0) * cos_w),
        (a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha,
    )
}

/// A high shelf: `gain_db` above `hz`, unity below.
pub fn high_shelf(hz: f64, q: f64, gain_db: f64, sample_rate: f64) -> BiquadCoeffs {
    let Rbj { cos_w, alpha } = Rbj::new(hz, q, sample_rate);
    let a = 10.0_f64.powf(gain_db / 40.0);
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
    BiquadCoeffs::normalise(
        a * ((a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha),
        -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w),
        a * ((a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha),
        (a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha,
        2.0 * ((a - 1.0) - (a + 1.0) * cos_w),
        (a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha,
    )
}

/// The two Qs a 24 dB/octave Butterworth response needs from a pair of
/// cascaded 12 dB/octave sections.
///
/// A fourth-order Butterworth's poles sit at ±22.5° and ±67.5° from the
/// negative real axis, which is `1/(2 cos θ)` = 0.5412 and 1.3066. Cascading two
/// sections both at `1/√2` would give a response 3 dB down at the corner twice
/// over, which is the usual way this is got wrong.
pub const BUTTERWORTH_Q4: [f64; 2] = [0.541_196_100_146_197, 1.306_562_964_876_377];

/// One second-order section's state, in transposed direct form II.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Biquad {
    s1: f64,
    s2: f64,
}

impl Biquad {
    /// A section with no memory.
    pub const fn new() -> Self {
        Self { s1: 0.0, s2: 0.0 }
    }

    /// Forget everything. Called when a chain is re-entered after a bypass, so
    /// it cannot ring with audio from before the user turned it off.
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }

    /// Whether the section is genuinely at rest.
    pub fn is_silent(&self) -> bool {
        self.s1 == 0.0 && self.s2 == 0.0
    }

    /// One sample.
    ///
    /// # RT safety
    /// Five multiplies, four adds and two compares. No allocation, no branches
    /// that depend on anything but the state's own magnitude.
    #[inline]
    pub fn process(&mut self, x: f64, c: &BiquadCoeffs) -> f64 {
        let y = c.b0 * x + self.s1;
        self.s1 = c.b1 * x - c.a1 * y + self.s2;
        self.s2 = c.b2 * x - c.a2 * y;
        // See the module docs: FTZ does not reliably cover this recursion.
        if self.s1.abs() < DENORMAL_FLOOR {
            self.s1 = 0.0;
        }
        if self.s2.abs() < DENORMAL_FLOOR {
            self.s2 = 0.0;
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    /// Magnitude in dB by *running* the filter: a long sine at `hz`, measured
    /// after the transient. The independent check on `magnitude_at`, which is
    /// closed-form and could be closed-form wrong.
    ///
    /// RMS and not peak. A sampled sine's largest *sample* is not its amplitude
    /// -- at 3 kHz there are sixteen samples per cycle and the best one can sit
    /// `cos(π/16)` below the crest, which is 0.17 dB of measurement error and
    /// exactly the size of the discrepancy this test is supposed to catch.
    fn measured_db(coeffs: &BiquadCoeffs, hz: f64) -> f64 {
        let mut filter = Biquad::new();
        let frames = 48_000;
        let settle = 8_000;
        let mut energy = 0.0_f64;
        for n in 0..frames {
            let x = (TAU * hz * n as f64 / SR).sin();
            let y = filter.process(x, coeffs);
            if n >= settle {
                energy += y * y;
            }
        }
        let rms = (energy / (frames - settle) as f64).sqrt();
        20.0 * (rms * std::f64::consts::SQRT_2).max(1e-12).log10()
    }

    fn db(linear: f64) -> f64 {
        20.0 * linear.max(1e-12).log10()
    }

    #[test]
    fn a_bell_puts_its_gain_exactly_at_its_centre_and_unity_far_from_it() {
        let coeffs = peaking(1_000.0, 1.0, 6.0, SR);
        assert!((db(coeffs.magnitude_at(1_000.0, SR)) - 6.0).abs() < 0.01);
        assert!(db(coeffs.magnitude_at(50.0, SR)).abs() < 0.1, "far below");
        assert!(
            db(coeffs.magnitude_at(16_000.0, SR)).abs() < 0.2,
            "far above"
        );

        // A cut is the mirror of a boost, which is what makes an EQ undoable.
        let cut = peaking(1_000.0, 1.0, -6.0, SR);
        assert!((db(cut.magnitude_at(1_000.0, SR)) + 6.0).abs() < 0.01);
    }

    #[test]
    fn the_closed_form_response_agrees_with_running_the_filter() {
        for (name, coeffs) in [
            ("bell +9", peaking(1_000.0, 2.0, 9.0, SR)),
            ("bell -12", peaking(300.0, 0.7, -12.0, SR)),
            ("low shelf", low_shelf(120.0, 0.707, 6.0, SR)),
            ("high shelf", high_shelf(6_000.0, 0.707, -6.0, SR)),
            ("highpass", highpass(200.0, 0.707, SR)),
        ] {
            for hz in [60.0, 200.0, 1_000.0, 3_000.0, 9_000.0] {
                let closed = db(coeffs.magnitude_at(hz, SR));
                let measured = measured_db(&coeffs, hz);
                assert!(
                    (closed - measured).abs() < 0.05,
                    "{name} at {hz} Hz: closed form {closed:.3} dB, measured {measured:.3} dB"
                );
            }
        }
    }

    #[test]
    fn a_highpass_is_three_db_down_at_its_corner_and_falls_twelve_db_an_octave() {
        let coeffs = highpass(1_000.0, std::f64::consts::FRAC_1_SQRT_2, SR);
        let corner = db(coeffs.magnitude_at(1_000.0, SR));
        assert!((corner + 3.0103).abs() < 0.02, "{corner}");

        // Two octaves below the corner, well inside the asymptote.
        let a = db(coeffs.magnitude_at(62.5, SR));
        let b = db(coeffs.magnitude_at(125.0, SR));
        assert!((b - a - 12.0).abs() < 0.3, "{a} -> {b} is not 12 dB/octave");
    }

    #[test]
    fn two_staggered_sections_make_a_flat_twenty_four_db_slope() {
        // The point of `BUTTERWORTH_Q4`: cascading two 0.707 sections would put
        // the corner 6 dB down instead of 3, and cascading two 24 dB sections
        // at the *same* Q would ripple.
        let first = highpass(1_000.0, BUTTERWORTH_Q4[0], SR);
        let second = highpass(1_000.0, BUTTERWORTH_Q4[1], SR);
        let at = |hz: f64| db(first.magnitude_at(hz, SR) * second.magnitude_at(hz, SR));

        assert!((at(1_000.0) + 3.0103).abs() < 0.05, "{}", at(1_000.0));
        // Flat in the passband: no resonant bump above the corner.
        for hz in [2_000.0, 4_000.0, 8_000.0] {
            assert!(at(hz).abs() < 0.05, "{hz} Hz: {}", at(hz));
        }
        // And 24 dB per octave below it.
        let slope = at(125.0) - at(62.5);
        assert!((slope - 24.0).abs() < 0.5, "{slope} dB/octave");
    }

    #[test]
    fn a_shelf_reaches_its_gain_and_leaves_the_other_end_alone() {
        let coeffs = low_shelf(200.0, 0.707, 12.0, SR);
        assert!(
            (db(coeffs.magnitude_at(20.0, SR)) - 12.0).abs() < 0.3,
            "shelf floor"
        );
        assert!(
            db(coeffs.magnitude_at(10_000.0, SR)).abs() < 0.05,
            "untouched above"
        );
        // The corner of a shelf is conventionally its *midpoint* gain.
        assert!((db(coeffs.magnitude_at(200.0, SR)) - 6.0).abs() < 0.1);

        let high = high_shelf(4_000.0, 0.707, -9.0, SR);
        assert!((db(high.magnitude_at(20_000.0, SR)) + 9.0).abs() < 0.3);
        assert!(db(high.magnitude_at(100.0, SR)).abs() < 0.05);
    }

    #[test]
    fn a_degenerate_request_is_a_pass_through_rather_than_a_nan() {
        // Everything here is reachable from a webview sending a number.
        for coeffs in [
            peaking(f64::NAN, 1.0, 6.0, SR),
            peaking(1_000.0, 0.0, 6.0, SR),
            highpass(1_000.0, 1.0, 0.0),
            lowpass(1e9, 1.0, SR),
        ] {
            let mut filter = Biquad::new();
            for _ in 0..64 {
                let y = filter.process(0.5, &coeffs);
                assert!(y.is_finite(), "{coeffs:?} produced {y}");
            }
        }
    }

    #[test]
    fn the_state_flushes_to_exactly_zero_rather_than_ringing_into_denormals() {
        let coeffs = lowpass(80.0, 0.707, SR);
        let mut filter = Biquad::new();
        for _ in 0..1_000 {
            filter.process(1.0, &coeffs);
        }
        assert!(!filter.is_silent());
        for _ in 0..200_000 {
            filter.process(0.0, &coeffs);
        }
        assert!(
            filter.is_silent(),
            "a filter fed silence must actually reach zero"
        );
    }

    #[test]
    fn a_reset_section_starts_from_nothing() {
        let coeffs = peaking(1_000.0, 1.0, 12.0, SR);
        let mut filter = Biquad::new();
        for n in 0..500 {
            filter.process((n as f64 * 0.01).sin(), &coeffs);
        }
        filter.reset();
        assert!(filter.is_silent());
        assert_eq!(filter.process(0.0, &coeffs), 0.0);
    }

    #[test]
    fn a_bypass_section_is_bit_transparent() {
        let mut filter = Biquad::new();
        for x in [0.5_f64, -0.25, 0.123_456_789] {
            assert_eq!(filter.process(x, &BiquadCoeffs::BYPASS), x);
        }
    }
}
