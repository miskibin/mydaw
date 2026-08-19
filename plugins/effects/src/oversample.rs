//! 4x oversampling for the saturator, as two cascaded 2x halfband stages.
//!
//! # What oversampling is for
//! A waveshaper generates harmonics. Harmonics above Nyquist do not vanish --
//! they fold back down into the audible band as inharmonic partials that no
//! filter afterwards can remove, because they are now sitting on top of the
//! music. Running the shaper at four times the rate moves the fold point four
//! times higher, and a lowpass before the decimation throws away everything
//! that would have folded.
//!
//! ```text
//!   x -> [up 2x] -> [up 2x] -> shaper -> [down 2x] -> [down 2x] -> y
//!         h(z)       h(z)                  h(z)        h(z)
//! ```
//!
//! # Why two 2x stages and not one 4x stage
//! A halfband filter -- one whose cutoff is exactly a quarter of its own sample
//! rate -- has **every even coefficient equal to zero** except the centre one.
//! That is half the multiplies for free, and it only happens at the halfband
//! ratio. Two halfband stages are therefore cheaper than one general 4x filter
//! *and* better, because the first stage only has to be clean up to the original
//! Nyquist while the second has nothing above it to protect against yet.
//!
//! # Why FIR and not the (cheaper) polyphase IIR
//! An IIR allpass halfband is dramatically cheaper for the same stopband and has
//! single-digit latency, and for a saturator on its own it would be the right
//! answer. It is the wrong answer *here* because the saturator has a **mix**
//! knob. An IIR halfband's group delay is frequency dependent, so the dry path
//! could not be delay-matched with an integer: blending 50/50 would comb the top
//! octave, which is exactly the "the mix knob mysteriously thins the highs"
//! failure users report. A linear-phase FIR has a delay that *is* an integer, so
//! the dry path is one delay line and the blend is phase-correct at every
//! setting. See [`Oversampler::latency`].
//!
//! # The filter, and why it is designed here rather than tabulated
//! The prototype is a windowed sinc at a quarter of the oversampled rate, with a
//! Kaiser window. Designing it in [`Halfband::new`] -- which runs on the
//! graph-building thread, where `powf` and Bessel functions are free -- rather
//! than pasting a table of coefficients means the *stopband is measurable* and
//! `tests/oversample.rs` measures it, instead of the code asserting a number
//! somebody else computed.

/// Half-length of the **outer** stage's prototype, in samples at its own
/// oversampled rate. The prototype is `2 * C + 1` taps with its centre at `C`.
///
/// This is the expensive stage: it runs at 2x the base rate and has to be clean
/// all the way from 20 kHz (where the music stops) to 28 kHz (where the first
/// image starts), which is a narrow transition and therefore a long filter. 32
/// puts the passband edge just under 20 kHz at a 48 kHz base rate; the test
/// measures it rather than trusting this comment.
///
/// **Must be even.** The centre tap then falls on an even index, which is what
/// makes the even polyphase branch a pure `C / 2` sample delay at the input rate
/// -- and therefore what makes the total latency an integer.
pub const OUTER_HALF_LENGTH: usize = 32;

/// Half-length of the **inner** stage's prototype.
///
/// Much shorter, and that is the whole economy of cascading: by the time the
/// inner stage runs there is nothing above 20 kHz left to protect, so its
/// transition band may run from 20 kHz all the way to 76 kHz. A quarter of the
/// taps for the same stopband.
pub const INNER_HALF_LENGTH: usize = 12;

/// Kaiser window parameter, shared by both stages.
///
/// `β ≈ 0.1102 (A - 8.7)` for a stopband attenuation `A` in dB, so 9.0 asks for
/// roughly 90 dB. `tests` measure what was actually obtained.
pub const KAISER_BETA: f64 = 9.0;

/// Zeroth-order modified Bessel function of the first kind.
///
/// The series converges quickly for the arguments a Kaiser window uses, and this
/// runs once per filter, off the audio thread.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..=32 {
        term *= (half / k as f64) * (half / k as f64);
        sum += term;
        if term < sum * 1.0e-17 {
            break;
        }
    }
    sum
}

/// The halfband prototype's odd taps, folded by symmetry.
///
/// A halfband filter's coefficients are zero at every **even** offset from the
/// centre, so only the odd ones are stored: `taps[j]` is `h[C + 2j + 1]`, which
/// by symmetry is also `h[C - 2j - 1]`. The centre tap is exactly `0.5` and is
/// applied as a delay rather than a multiply.
#[derive(Debug, Clone)]
struct Prototype {
    taps: Box<[f32]>,
}

impl Prototype {
    /// Design the window, normalise it to unity DC gain, and fold it.
    ///
    /// **Non-RT**: allocates and calls transcendentals.
    fn new(half_length: usize, beta: f64) -> Self {
        // Even, or the even branch is not a whole-sample delay. Rounding up
        // rather than asserting: this is a constant in this file, and a wrong
        // one should be a slightly longer filter, not a panic in a factory.
        let c = half_length.max(2) & !1;
        let centre = c as f64;
        let denominator = bessel_i0(beta);
        let full: Vec<f64> = (0..=2 * c)
            .map(|n| {
                let offset = n as f64 - centre;
                // The ideal halfband: a sinc cutting at a quarter of the rate.
                let ideal = if offset == 0.0 {
                    0.5
                } else {
                    (std::f64::consts::PI * offset * 0.5).sin() / (std::f64::consts::PI * offset)
                };
                let ratio = offset / centre;
                let window = bessel_i0(beta * (1.0 - ratio * ratio).max(0.0).sqrt()) / denominator;
                ideal * window
            })
            .collect();
        // Exactly unity DC gain. A halfband whose taps sum to 1.002 is an
        // oversampler with a 0.02 dB gain error per stage, which is how a
        // "bypassed" saturator ends up not quite bypassed.
        let sum: f64 = full.iter().sum();
        let scale = if sum.abs() > 1.0e-12 { 1.0 / sum } else { 1.0 };
        // Odd offsets from the centre: +1, +3, ... +(C-1). There are C/2 of
        // them, each standing for a symmetric pair.
        let taps: Vec<f32> = (0..c / 2)
            .map(|j| (full[c + 2 * j + 1] * scale) as f32)
            .collect();
        Self {
            taps: taps.into_boxed_slice(),
        }
    }

    /// Symmetric tap pairs, `C / 2`. Also the group delay of one direction, in
    /// samples at the **input** rate.
    fn pairs(&self) -> usize {
        self.taps.len()
    }
}

/// A ring of `f32` with the newest sample at age zero.
#[derive(Debug)]
struct History {
    buffer: Box<[f32]>,
    write: usize,
}

impl History {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)].into_boxed_slice(),
            write: 0,
        }
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }

    #[inline]
    fn push(&mut self, x: f32) {
        self.buffer[self.write] = x;
        self.write += 1;
        if self.write == self.buffer.len() {
            self.write = 0;
        }
    }

    /// The sample pushed `age` pushes ago; `age == 0` is the newest.
    #[inline]
    fn at(&self, age: usize) -> f32 {
        let len = self.buffer.len();
        self.buffer[(self.write + len - 1 - age % len) % len]
    }
}

/// One 2x halfband stage, usable in both directions at once.
///
/// # The polyphase decomposition, written out
/// With a prototype `h` of `2C + 1` taps centred at `C` (even), `D = C / 2`, and
/// `taps[j] = h[C + 2j + 1]`:
///
/// ```text
/// up:    even[i] = x[i - D]
///        odd[i]  = 2 · Σ_j taps[j] · ( x[i - D + 1 + j] + x[i - D - j] )
///
/// down:  y[i]    = ½ · even[i - D]
///                + Σ_j taps[j] · ( odd[i - D + j] + odd[i - D - 1 - j] )
/// ```
///
/// The even branch of the upsampler is a *pure delay* because every even
/// coefficient other than the centre is zero -- that is the halfband property,
/// and it is why this costs half of what a general 2x filter would.
#[derive(Debug)]
struct Halfband {
    prototype: Prototype,
    up_history: History,
    down_even: History,
    down_odd: History,
}

impl Halfband {
    /// **Non-RT**: allocates.
    fn new(half_length: usize, beta: f64) -> Self {
        let prototype = Prototype::new(half_length, beta);
        let d = prototype.pairs();
        Self {
            prototype,
            // Ages 0 ..= 2D - 1.
            up_history: History::new(2 * d),
            // Ages 1 ..= 2D.
            down_even: History::new(2 * d + 2),
            down_odd: History::new(2 * d + 2),
        }
    }

    fn reset(&mut self) {
        self.up_history.clear();
        self.down_even.clear();
        self.down_odd.clear();
    }

    /// Group delay of **one** direction, in samples at this stage's input rate.
    fn latency_in(&self) -> usize {
        self.prototype.pairs()
    }

    /// One input sample in, two oversampled samples out.
    #[inline]
    fn up(&mut self, x: f32) -> [f32; 2] {
        self.up_history.push(x);
        let d = self.prototype.pairs();
        let even = self.up_history.at(d);
        let mut odd = 0.0_f32;
        for (j, tap) in self.prototype.taps.iter().enumerate() {
            odd += tap * (self.up_history.at(d - 1 - j) + self.up_history.at(d + j));
        }
        // The gain zero-stuffing threw away.
        [even, odd * 2.0]
    }

    /// Two oversampled samples in, one input sample out.
    #[inline]
    fn down(&mut self, pair: [f32; 2]) -> f32 {
        self.down_even.push(pair[0]);
        self.down_odd.push(pair[1]);
        let d = self.prototype.pairs();
        let mut sum = 0.5 * self.down_even.at(d);
        for (j, tap) in self.prototype.taps.iter().enumerate() {
            sum += tap * (self.down_odd.at(d - j) + self.down_odd.at(d + 1 + j));
        }
        sum
    }
}

/// Oversampling factors the saturator offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OversampleFactor {
    /// No oversampling, and therefore no latency. The honest "off", and also
    /// what the aliasing test measures 4x *against*.
    Off,
    /// One halfband stage.
    Two,
    /// Two cascaded halfband stages. The default: the research is that filter
    /// quality beats ratio, and 4x with a 90 dB halfband is comfortably past the
    /// point where a `tanh`'s residual folding is measurable.
    Four,
}

impl OversampleFactor {
    /// The integer ratio.
    pub const fn ratio(self) -> usize {
        match self {
            Self::Off => 1,
            Self::Two => 2,
            Self::Four => 4,
        }
    }

    /// The factor a switch position selects. Anything unexpected is 4x, the
    /// default, rather than a panic on the audio thread.
    pub const fn from_choice(choice: usize) -> Self {
        match choice {
            0 => Self::Off,
            1 => Self::Two,
            _ => Self::Four,
        }
    }
}

/// Up to 4x oversampling around a per-sample nonlinearity.
///
/// # RT safety
/// [`process`](Self::process) is multiply-accumulate over pre-allocated rings
/// plus a call into the caller's closure. Everything it touches was allocated in
/// [`new`](Self::new).
#[derive(Debug)]
pub struct Oversampler {
    outer: Halfband,
    inner: Halfband,
    factor: OversampleFactor,
}

impl Oversampler {
    /// An oversampler at `factor`.
    ///
    /// **Non-RT**: allocates and designs both filters. Both are built whatever
    /// the factor, so [`set_factor`](Self::set_factor) never has to allocate.
    pub fn new(factor: OversampleFactor) -> Self {
        Self {
            outer: Halfband::new(OUTER_HALF_LENGTH, KAISER_BETA),
            inner: Halfband::new(INNER_HALF_LENGTH, KAISER_BETA),
            factor,
        }
    }

    /// The factor in force.
    pub const fn factor(&self) -> OversampleFactor {
        self.factor
    }

    /// Switch factors, clearing the filters.
    ///
    /// The latency changes with the factor, so a caller with a delay-matched dry
    /// path has to re-read [`latency`](Self::latency) afterwards.
    pub fn set_factor(&mut self, factor: OversampleFactor) {
        if factor != self.factor {
            self.factor = factor;
            self.reset();
        }
    }

    /// Empty every filter, so a bypassed effect comes back with no tail.
    pub fn reset(&mut self) {
        self.outer.reset();
        self.inner.reset();
    }

    /// Round-trip latency, in samples at the base rate.
    ///
    /// Zero when off, which is what makes "off" genuinely free rather than
    /// merely cheaper. The dry path of a mix knob must be delayed by exactly
    /// this much, and it can be, because a linear-phase FIR's delay is an
    /// integer -- see the module docs on why that decided FIR over IIR.
    pub fn latency(&self) -> usize {
        match self.factor {
            OversampleFactor::Off => 0,
            // Up and down each contribute the prototype's delay.
            OversampleFactor::Two => 2 * self.outer.latency_in(),
            // The inner stage runs at 2x, so its round trip is half as long
            // measured at the base rate.
            OversampleFactor::Four => 2 * self.outer.latency_in() + self.inner.latency_in(),
        }
    }

    /// Run `shape` at the oversampled rate for one input sample.
    ///
    /// `shape` is called [`ratio`](OversampleFactor::ratio) times per input
    /// sample and must be a pure per-sample function -- it is the only thing
    /// that may be nonlinear inside the oversampled region.
    #[inline]
    pub fn process(&mut self, x: f32, mut shape: impl FnMut(f32) -> f32) -> f32 {
        match self.factor {
            OversampleFactor::Off => shape(x),
            OversampleFactor::Two => {
                let [a, b] = self.outer.up(x);
                self.outer.down([shape(a), shape(b)])
            }
            OversampleFactor::Four => {
                let [a, b] = self.outer.up(x);
                // Time order matters: `a` precedes `b`, and both stages keep
                // history.
                let [a0, a1] = self.inner.up(a);
                let first = self.inner.down([shape(a0), shape(a1)]);
                let [b0, b1] = self.inner.up(b);
                let second = self.inner.down([shape(b0), shape(b1)]);
                self.outer.down([first, second])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const SR: f32 = 48_000.0;

    /// Peak magnitude at the output of a halfband's up-then-down round trip for
    /// a sine at `hz`, in dB relative to the input.
    fn round_trip_db(factor: OversampleFactor, hz: f32) -> f32 {
        let mut over = Oversampler::new(factor);
        let frames = 48_000;
        let settle = frames / 2;
        let mut energy = 0.0_f64;
        for n in 0..frames {
            let x = (TAU * hz * n as f32 / SR).sin();
            let y = over.process(x, |s| s);
            if n >= settle {
                energy += f64::from(y).powi(2);
            }
        }
        // RMS, not peak. At 16 kHz there are three samples per cycle and the
        // best one can sit 6 dB below the crest -- a measurement error far
        // larger than the passband ripple this is checking.
        let rms = (energy / (frames - settle) as f64).sqrt();
        (20.0 * (rms * std::f64::consts::SQRT_2).max(1e-12).log10()) as f32
    }

    #[test]
    fn a_round_trip_with_no_shaper_is_flat_across_the_audio_band() {
        // The property everything else rests on: oversampling a *linear*
        // function must be inaudible, or the saturator would colour the signal
        // before it distorted it.
        for factor in [OversampleFactor::Two, OversampleFactor::Four] {
            for hz in [100.0_f32, 1_000.0, 5_000.0, 12_000.0, 16_000.0, 18_000.0] {
                let db = round_trip_db(factor, hz);
                assert!(
                    db.abs() < 0.15,
                    "{factor:?} at {hz} Hz: {db} dB is not flat"
                );
            }
        }
    }

    #[test]
    fn the_stopband_is_deep_enough_to_bury_what_folds_into_it() {
        // Measured, not asserted from a comment: feed the *oversampled* rate a
        // tone above the base Nyquist and see how much survives decimation.
        // A tone at 0.35 of the oversampled rate would alias to 0.15 without
        // the filter.
        let mut over = Oversampler::new(OversampleFactor::Two);
        let mut energy = 0.0_f64;
        let frames = 48_000;
        // The shaper slot is where the oversampled-rate signal is generated,
        // which is exactly how a harmonic above Nyquist gets there in practice.
        let mut phase = 0.0_f32;
        for n in 0..frames {
            let y = over.process(0.0, |_| {
                // 0.35 of the oversampled rate, well inside the stopband.
                phase = (phase + 0.35) % 1.0;
                (TAU * phase).sin()
            });
            if n > frames / 2 {
                energy += f64::from(y).powi(2);
            }
        }
        let rms = (energy / (frames / 2) as f64).sqrt();
        let db = 20.0 * (rms * std::f64::consts::SQRT_2).max(1e-14).log10();
        assert!(
            db < -80.0,
            "the halfband only rejects {db} dB; raise HALF_LENGTH or KAISER_BETA"
        );
    }

    #[test]
    fn latency_is_an_integer_number_of_base_rate_samples_and_zero_when_off() {
        // The whole reason for choosing FIR: the dry path of the mix knob is a
        // plain delay line, and it can only be that if this is an integer.
        let off = Oversampler::new(OversampleFactor::Off);
        assert_eq!(off.latency(), 0);
        let two = Oversampler::new(OversampleFactor::Two);
        let four = Oversampler::new(OversampleFactor::Four);
        assert!(two.latency() > 0);
        assert!(
            four.latency() > two.latency(),
            "a second stage cannot be free"
        );
        // Under a millisecond at 48 kHz, which is the budget an insert has.
        assert!(
            four.latency() <= 40,
            "{} samples is too much for a channel insert",
            four.latency()
        );
        // The exact numbers, so a change to either half-length is a decision
        // rather than a surprise: the docs quote them and the saturator's dry
        // path is delayed by them.
        assert_eq!(two.latency(), OUTER_HALF_LENGTH);
        assert_eq!(four.latency(), OUTER_HALF_LENGTH + INNER_HALF_LENGTH / 2);
    }

    #[test]
    fn the_reported_latency_is_where_an_impulse_actually_comes_out() {
        for factor in [OversampleFactor::Two, OversampleFactor::Four] {
            let mut over = Oversampler::new(factor);
            let mut best = (0_usize, 0.0_f32);
            for n in 0..512 {
                let x = if n == 0 { 1.0 } else { 0.0 };
                let y = over.process(x, |s| s);
                if y.abs() > best.1 {
                    best = (n, y.abs());
                }
            }
            assert_eq!(
                best.0,
                over.latency(),
                "{factor:?}: the peak of the impulse is not at the reported latency"
            );
        }
    }

    #[test]
    fn off_is_bit_transparent_and_costs_nothing() {
        let mut over = Oversampler::new(OversampleFactor::Off);
        for x in [0.5_f32, -0.25, 0.123_456_79] {
            assert_eq!(over.process(x, |s| s), x);
        }
        assert_eq!(over.latency(), 0);
    }

    #[test]
    fn oversampling_actually_suppresses_a_shapers_aliasing() {
        // The claim the feature exists to make, measured end to end: a hard
        // nonlinearity on a high sine puts inharmonic energy in the band, and
        // 4x must put much less of it there than no oversampling at all.
        //
        // 7 kHz through a hard clipper: the third harmonic (21 kHz) is still
        // under Nyquist, but the fifth (35 kHz) folds to 13 kHz and the seventh
        // (49 kHz) folds to 1 kHz -- all inharmonic, all audible.
        let alias_energy = |factor: OversampleFactor| {
            let mut over = Oversampler::new(factor);
            let mut out = Vec::with_capacity(8_192);
            for n in 0..12_000 {
                let x = 0.9 * (TAU * 7_000.0 * n as f32 / SR).sin();
                let y = over.process(x, |s| s.clamp(-0.3, 0.3));
                if n >= 4_000 {
                    out.push(y);
                }
            }
            // Energy around 1 kHz, where nothing harmonic can be: the folded
            // seventh is the only thing that can put energy there.
            let mut real = 0.0_f32;
            let mut imag = 0.0_f32;
            for (n, s) in out.iter().enumerate() {
                let w = TAU * 1_000.0 * n as f32 / SR;
                real += s * w.cos();
                imag += s * w.sin();
            }
            (real * real + imag * imag).sqrt() / out.len() as f32
        };

        let none = alias_energy(OversampleFactor::Off);
        let four = alias_energy(OversampleFactor::Four);
        let suppression = 20.0 * (none / four.max(1e-12)).log10();
        assert!(
            suppression > 20.0,
            "4x only bought {suppression:.1} dB of alias suppression"
        );
    }

    #[test]
    fn a_reset_oversampler_has_nothing_left_in_its_filters() {
        let mut over = Oversampler::new(OversampleFactor::Four);
        for _ in 0..256 {
            over.process(1.0, |s| s);
        }
        over.reset();
        // The first sample after a reset is the filter's leading tap alone,
        // which is small; what matters is that nothing from before survives.
        let mut energy = 0.0_f32;
        for _ in 0..over.latency() {
            energy += over.process(0.0, |s| s).abs();
        }
        assert!(energy < 1e-6, "{energy} of the old signal survived a reset");
    }

    #[test]
    fn the_prototype_has_unity_dc_gain_so_a_bypassed_shaper_is_not_a_trim() {
        for half in [OUTER_HALF_LENGTH, INNER_HALF_LENGTH] {
            let prototype = Prototype::new(half, KAISER_BETA);
            assert_eq!(prototype.pairs(), half / 2, "C/2 symmetric odd pairs");
            // Centre tap 0.5 plus twice the folded odd taps is the whole sum.
            let sum: f32 = 0.5 + 2.0 * prototype.taps.iter().sum::<f32>();
            // Summed in `f32`, so the tolerance is rounding and not design.
            assert!((sum - 1.0).abs() < 1e-4, "DC gain of C={half} is {sum}");
        }
    }
}
