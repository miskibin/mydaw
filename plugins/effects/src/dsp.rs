//! The primitives the five effects are built from.
//!
//! Everything here is legal inside [`RtNode::process`](mydaw_rt_core::RtNode):
//! no allocation, no locks, no branches on anything but its own state.
//! Everything expensive -- an `exp`, a `powf`, a division -- happens in a
//! constructor or in a `set_*` that a node calls at most once per block.
//!
//! [`biquad`](crate::biquad) has the second-order sections; this module has the
//! one-poles, the shapers, the followers and the delay line.

use std::f32::consts::TAU;

/// Convert decibels to a linear multiplier.
///
/// Unlike [`mydaw_rt_core::db_to_gain`], which floors at the fader's -60 dB
/// silence point, this is the plain conversion: an effect's -60 dB is a real
/// -60 dB and not a mute.
pub fn db_to_linear(db: f32) -> f32 {
    if db.is_finite() {
        10.0_f32.powf(db / 20.0)
    } else {
        0.0
    }
}

/// Convert a linear multiplier to decibels, floored at -120 dB.
pub fn linear_to_db(linear: f32) -> f32 {
    if linear.is_finite() && linear > 1.0e-6 {
        20.0 * linear.log10()
    } else {
        -120.0
    }
}

/// Rate substituted for one that cannot be divided by.
pub const FALLBACK_SAMPLE_RATE: f64 = 48_000.0;

/// A sample rate that can be divided by, substituting [`FALLBACK_SAMPLE_RATE`]
/// for one that cannot.
///
/// The same defence the instruments make, and for the same reason: a device
/// reporting 0 Hz is a bug somewhere else, and the audio thread is the worst
/// place to find out.
pub fn usable_sample_rate(sample_rate: f64) -> f32 {
    let rate = sample_rate as f32;
    if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        FALLBACK_SAMPLE_RATE as f32
    }
}

/// A one-pole filter in topology-preserving transposed (TPT) form.
///
/// # Why TPT and not the naive one-pole
/// The naive `y += a(x - y)` is fine until its coefficient is modulated, at
/// which point its cutoff drifts from where the maths says it is -- exactly what
/// a delay's damping knob and a saturator's tone control do. The TPT form's
/// cutoff is `tan(π f / fs)` and stays correct while the coefficient moves,
/// which is what makes "the damping filter is at 6 kHz" a statement a test can
/// check while the knob is being dragged.
///
/// Both outputs come from the same state: the lowpass is `y`, and the highpass
/// is `x - y`. 6 dB/octave, which is the right slope inside a feedback loop --
/// a steeper filter rings, and a ring inside a loop compounds.
#[derive(Debug, Clone, Copy)]
pub struct OnePoleTpt {
    /// `g / (1 + g)`, precomputed.
    coefficient: f32,
    state: f32,
}

impl Default for OnePoleTpt {
    fn default() -> Self {
        Self::new()
    }
}

impl OnePoleTpt {
    /// A filter wide open, with no memory.
    pub const fn new() -> Self {
        Self {
            coefficient: 1.0,
            state: 0.0,
        }
    }

    /// Set the -3 dB point, in Hz.
    ///
    /// Cheap enough (one `tan`) to call once per block; not cheap enough to call
    /// per sample, which is why the delay's damping recomputes at block rate.
    pub fn set_cutoff(&mut self, hz: f32, sample_rate: f32) {
        self.coefficient = Self::coefficient_for(hz, sample_rate);
    }

    /// The coefficient [`set_cutoff`](Self::set_cutoff) would compute for `hz`.
    ///
    /// For a caller that shares one cutoff across several filters, or glides a
    /// coefficient per sample toward a block-rate target: the `tan` runs once.
    pub fn coefficient_for(hz: f32, sample_rate: f32) -> f32 {
        let nyquist = sample_rate * 0.5;
        let hz = hz.clamp(1.0, nyquist * 0.99);
        let g = (std::f32::consts::PI * hz / sample_rate).tan();
        g / (1.0 + g)
    }

    /// Install a coefficient from [`coefficient_for`](Self::coefficient_for).
    pub fn set_coefficient(&mut self, coefficient: f32) {
        self.coefficient = coefficient;
    }

    /// Forget the filter's memory.
    pub fn reset(&mut self) {
        self.state = 0.0;
    }

    /// The lowpass output for one sample.
    #[inline]
    pub fn low(&mut self, x: f32) -> f32 {
        let v = (x - self.state) * self.coefficient;
        let y = v + self.state;
        self.state = y + v;
        // The audio thread runs with FTZ, but a filter fed silence for minutes
        // should be *at* zero rather than near it, so a bypass can be exact.
        if self.state.abs() < 1.0e-20 {
            self.state = 0.0;
        }
        y
    }

    /// The highpass output for one sample: the input minus its lowpass.
    #[inline]
    pub fn high(&mut self, x: f32) -> f32 {
        x - self.low(x)
    }

    /// The coefficient, for tests.
    pub const fn coefficient(&self) -> f32 {
        self.coefficient
    }
}

/// A first-order DC blocker: `y[n] = x[n] - x[n-1] + R·y[n-1]`.
///
/// Julius Smith's canonical form. `R` is derived from a cutoff in Hz rather than
/// hard-coded, so the filter stays at the same *frequency* at 96 kHz instead of
/// sliding down an octave -- which is the usual way a hard-coded `0.995` goes
/// wrong.
///
/// 5 Hz by default: low enough to be inaudible on a sub-bass line, high enough
/// to settle in about 80 ms. A blocker at 20 Hz would be audible on a kick, and
/// its phase shift reaches much further up than its amplitude response does.
#[derive(Debug, Clone, Copy)]
pub struct DcBlocker {
    r: f32,
    x1: f32,
    y1: f32,
}

impl Default for DcBlocker {
    fn default() -> Self {
        Self::new(DcBlocker::DEFAULT_HZ, 48_000.0)
    }
}

impl DcBlocker {
    /// Corner frequency a DC blocker uses unless told otherwise.
    pub const DEFAULT_HZ: f32 = 5.0;

    /// A blocker with a -3 dB point at `hz`.
    pub fn new(hz: f32, sample_rate: f32) -> Self {
        let mut blocker = Self {
            r: 0.995,
            x1: 0.0,
            y1: 0.0,
        };
        blocker.set_cutoff(hz, sample_rate);
        blocker
    }

    /// Move the corner.
    pub fn set_cutoff(&mut self, hz: f32, sample_rate: f32) {
        let hz = hz.clamp(0.1, sample_rate * 0.1);
        self.r = (1.0 - TAU * hz / sample_rate).clamp(0.0, 0.999_99);
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }

    /// One sample.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = if y.abs() < 1.0e-20 { 0.0 } else { y };
        y
    }
}

/// A soft clipper with unity slope at the origin: `tanh`, cheaply.
///
/// The rational approximation `x(27 + x²)/(27 + 9x²)` -- the (3,2) Padé
/// approximant of `tanh` -- costs three multiplies and a divide instead of two
/// exponentials, and tracks `tanh` closely everywhere. It is not trying
/// to *be* `tanh`: what the shaper needs is a curve that is odd, monotone,
/// bounded, smooth, and has unity slope at the origin, and this has all five.
/// The worst-case deviation, measured in the tests, is 0.024.
/// Beyond `|x| = 3` it is clamped, which is where `tanh` is flat anyway.
///
/// Odd-symmetric, so it produces **odd harmonics only** -- the third, fifth and
/// seventh -- which is what "tape-ish" means and what
/// `tests/saturator_dsp.rs` asserts.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

/// An asymmetric shaper: soft on the way up, softer on the way down.
///
/// A single-ended valve stage clips its two halves differently, and *that* --
/// not the amount of clipping -- is what puts a second harmonic in the output.
/// Biasing the input, shaping, and removing the resulting DC is the standard
/// way to get it; the DC has to go, or the effect writes an offset into the mix.
#[inline]
pub fn asymmetric_clip(x: f32) -> f32 {
    // The positive half saturates sooner, so the transfer curve is not odd.
    if x >= 0.0 {
        soft_clip(x * 1.6) * 0.75
    } else {
        soft_clip(x * 0.8)
    }
}

/// A hard clipper with a short soft knee.
///
/// Not `x.clamp(-1, 1)`: a corner that sharp puts energy at every frequency and
/// aliases spectacularly even at 4x oversampling. The quadratic knee over the
/// top 1/3 keeps the first derivative continuous, which is audibly and
/// measurably less harsh for the same loudness.
#[inline]
pub fn hard_clip(x: f32) -> f32 {
    const KNEE: f32 = 2.0 / 3.0;
    let a = x.abs();
    if a <= KNEE {
        x
    } else if a >= 1.0 {
        x.signum()
    } else {
        let t = (a - KNEE) / (1.0 - KNEE);
        x.signum() * (KNEE + (1.0 - KNEE) * (t - t * t / 2.0) / 0.5 * 0.5)
    }
}

/// An envelope follower with separate attack and release coefficients.
///
/// Coefficients are `exp(-1 / (τ · fs))`, the standard one-pole form, so a time
/// constant of `t` milliseconds means the envelope covers `1 - 1/e` (63%) of the
/// distance in `t` milliseconds. That is the convention hardware compressors
/// were specified with and the one a test can check against a number.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeFollower {
    attack: f32,
    release: f32,
    envelope: f32,
}

impl Default for EnvelopeFollower {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvelopeFollower {
    /// A follower at rest, with instantaneous attack and release.
    pub const fn new() -> Self {
        Self {
            attack: 0.0,
            release: 0.0,
            envelope: 0.0,
        }
    }

    /// The one-pole coefficient for a time constant of `ms`.
    ///
    /// Zero for a degenerate time, which is an instant response rather than a
    /// pole on the unit circle.
    pub fn coefficient(ms: f32, sample_rate: f32) -> f32 {
        if !ms.is_finite() || ms <= 0.0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
            return 0.0;
        }
        (-1_000.0 / (ms * sample_rate)).exp()
    }

    /// Set both time constants, in milliseconds.
    pub fn set_times(&mut self, attack_ms: f32, release_ms: f32, sample_rate: f32) {
        self.attack = Self::coefficient(attack_ms, sample_rate);
        self.release = Self::coefficient(release_ms, sample_rate);
    }

    /// Set the coefficients directly, for a caller that already has them.
    pub fn set_coefficients(&mut self, attack: f32, release: f32) {
        self.attack = attack.clamp(0.0, 1.0);
        self.release = release.clamp(0.0, 1.0);
    }

    pub fn reset(&mut self) {
        self.envelope = 0.0;
    }

    /// The envelope right now.
    pub const fn value(&self) -> f32 {
        self.envelope
    }

    /// Follow `input` for one sample, attacking when it rises and releasing when
    /// it falls.
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let coefficient = if input > self.envelope {
            self.attack
        } else {
            self.release
        };
        self.envelope = input + coefficient * (self.envelope - input);
        if self.envelope.abs() < 1.0e-20 {
            self.envelope = 0.0;
        }
        self.envelope
    }
}

/// A fractional-delay line with cubic Hermite interpolation.
///
/// # Why cubic and not linear
/// The delay's time knob and its modulation both move the read pointer between
/// samples. Linear interpolation between two neighbours is a lowpass whose
/// cutoff depends on the fractional part, so a modulated delay would swing in
/// and out of dullness at the modulation rate -- audible as a warble that is not
/// the warble anyone asked for. Four-point Hermite is flat to within a fraction
/// of a decibel across the top octave and costs eight multiplies.
///
/// The buffer is allocated once, in [`new`](Self::new), and never grows.
#[derive(Debug)]
pub struct DelayLine {
    buffer: Box<[f32]>,
    write: usize,
}

impl DelayLine {
    /// Shortest delay a line will serve. See [`read`](Self::read).
    pub const MIN_DELAY: f32 = 2.0;

    /// A line that can hold `frames` samples of history.
    ///
    /// **Non-RT**: this allocates. Four extra slots are added so that a read at
    /// the maximum delay still has the two neighbours Hermite needs on either
    /// side without wrapping onto the sample being written.
    pub fn new(frames: usize) -> Self {
        let len = frames.max(4) + 4;
        Self {
            buffer: vec![0.0; len].into_boxed_slice(),
            write: 0,
        }
    }

    /// Longest delay this line can produce, in samples.
    pub fn capacity(&self) -> usize {
        self.buffer.len() - 4
    }

    /// Empty the line. A bypassed delay comes back with no stale echoes.
    pub fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }

    /// Whether the line holds nothing at all.
    pub fn is_silent(&self) -> bool {
        self.buffer.iter().all(|s| *s == 0.0)
    }

    /// Store one sample and advance the write pointer.
    #[inline]
    pub fn write(&mut self, x: f32) {
        self.buffer[self.write] = x;
        self.write += 1;
        if self.write == self.buffer.len() {
            self.write = 0;
        }
    }

    /// Read `delay` samples back, interpolating between neighbours.
    ///
    /// `delay` is clamped into [`MIN_DELAY`](Self::MIN_DELAY)`..= capacity()`.
    /// The floor is two rather than one because Hermite needs a sample *newer*
    /// than the pair it interpolates between: at a delay of one there is no such
    /// sample, only the slot about to be overwritten. Two samples is 42 µs at
    /// 48 kHz, which is below anything a delay's time control can ask for
    /// anyway.
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        let len = self.buffer.len();
        let delay = if delay.is_finite() {
            delay.clamp(Self::MIN_DELAY, self.capacity() as f32)
        } else {
            Self::MIN_DELAY
        };
        let whole = delay.floor() as usize;
        let frac = delay - whole as f32;
        // `write` points at the slot about to be written, so the sample `d`
        // samples ago is at `write - d`.
        let at = |d: usize| self.buffer[(self.write + len - d) % len];
        // `ym1` is one sample *newer* than the pair, `y2` one older.
        hermite(frac, at(whole - 1), at(whole), at(whole + 1), at(whole + 2))
    }
}

/// Four-point, third-order Hermite interpolation at fractional position `t`
/// between `y0` and `y1`.
#[inline]
pub fn hermite(t: f32, ym1: f32, y0: f32, y1: f32, y2: f32) -> f32 {
    let c0 = y0;
    let c1 = 0.5 * (y1 - ym1);
    let c2 = ym1 - 2.5 * y0 + 2.0 * y1 - 0.5 * y2;
    let c3 = 0.5 * (y2 - ym1) + 1.5 * (y0 - y1);
    ((c3 * t + c2) * t + c1) * t + c0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    #[test]
    fn decibels_convert_both_ways_without_the_faders_silence_floor() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_linear(6.0206) - 2.0).abs() < 1e-4);
        assert!((db_to_linear(-6.0206) - 0.5).abs() < 1e-5);
        // An effect's -60 dB is a real level, not a mute: this is the one place
        // it differs from the mixer's fader conversion.
        assert!(db_to_linear(-60.0) > 0.0);
        assert!((linear_to_db(db_to_linear(-24.0)) + 24.0).abs() < 1e-3);
    }

    /// Magnitude of a filter at `hz`, measured by running a sine through it.
    fn response_db(mut filter: OnePoleTpt, hz: f32, high: bool) -> f32 {
        let frames = 96_000;
        let mut peak = 0.0_f32;
        for n in 0..frames {
            let x = (TAU * hz * n as f32 / SR).sin();
            let y = if high { filter.high(x) } else { filter.low(x) };
            if n > 24_000 {
                peak = peak.max(y.abs());
            }
        }
        20.0 * peak.max(1e-9).log10()
    }

    #[test]
    fn a_one_pole_is_three_db_down_at_its_cutoff_and_six_db_an_octave_beyond() {
        let mut filter = OnePoleTpt::new();
        filter.set_cutoff(1_000.0, SR);
        assert!((response_db(filter, 1_000.0, false) + 3.0103).abs() < 0.1);
        let a = response_db(filter, 4_000.0, false);
        let b = response_db(filter, 8_000.0, false);
        assert!((a - b - 6.0).abs() < 0.3, "{a} -> {b}");

        // The complementary output is the highpass, at the same corner.
        assert!((response_db(filter, 1_000.0, true) + 3.0103).abs() < 0.1);
        assert!(response_db(filter, 60.0, true) < -20.0, "well below");
    }

    #[test]
    fn a_dc_blocker_removes_a_constant_and_leaves_the_audio_band_alone() {
        let mut blocker = DcBlocker::new(DcBlocker::DEFAULT_HZ, SR);
        let mut last = 0.0;
        for _ in 0..SR as usize {
            last = blocker.process(0.5);
        }
        assert!(
            last.abs() < 1e-3,
            "a constant should decay away, got {last}"
        );

        // 100 Hz is twenty octaves-ish above 5 Hz: untouched.
        let mut blocker = DcBlocker::new(DcBlocker::DEFAULT_HZ, SR);
        let mut peak = 0.0_f32;
        for n in 0..48_000 {
            let y = blocker.process((TAU * 100.0 * n as f32 / SR).sin());
            if n > 12_000 {
                peak = peak.max(y.abs());
            }
        }
        assert!((peak - 1.0).abs() < 0.02, "100 Hz came out at {peak}");
    }

    #[test]
    fn a_dc_blockers_corner_does_not_slide_with_the_sample_rate() {
        // The point of deriving R from a frequency: a hard-coded 0.995 is 3.5 Hz
        // at 44.1 kHz and 1.6 Hz at 96 kHz, which is a different filter.
        let a = DcBlocker::new(5.0, 44_100.0).r;
        let b = DcBlocker::new(5.0, 96_000.0).r;
        assert!(b > a, "a higher rate needs a pole closer to 1");
        let corner = |r: f32, sr: f32| (1.0 - r) * sr / TAU;
        assert!((corner(a, 44_100.0) - 5.0).abs() < 0.1);
        assert!((corner(b, 96_000.0) - 5.0).abs() < 0.1);
    }

    #[test]
    fn the_soft_clipper_has_unity_slope_at_the_origin_and_is_odd() {
        assert_eq!(soft_clip(0.0), 0.0);
        // Unity slope: a quiet signal passes through essentially untouched,
        // which is what makes a drive knob at the bottom transparent.
        let slope = (soft_clip(1e-3) - soft_clip(-1e-3)) / 2e-3;
        assert!((slope - 1.0).abs() < 1e-4, "{slope}");
        for x in [0.3_f32, 0.9, 1.5, 2.5] {
            assert!((soft_clip(x) + soft_clip(-x)).abs() < 1e-6, "odd at {x}");
            assert!(soft_clip(x) < x, "it must actually compress at {x}");
        }
        // Bounded, whatever it is handed.
        assert!(soft_clip(1e6).abs() <= 1.0);
        assert!(soft_clip(-1e6).abs() <= 1.0);
    }

    #[test]
    fn the_soft_clipper_tracks_tanh_closely_enough_to_stand_in_for_it() {
        let mut worst = 0.0_f32;
        for step in -60..=60 {
            let x = step as f32 / 20.0;
            worst = worst.max((soft_clip(x) - x.tanh()).abs());
        }
        assert!(worst < 0.025, "the Padé approximant is {worst} off tanh");
        // Monotone, which the harmonic structure depends on: a shaper that
        // folded back would make a wavefolder's spectrum, not a tape's.
        let mut previous = soft_clip(-4.0);
        for step in -399..=400 {
            let y = soft_clip(step as f32 / 100.0);
            assert!(y >= previous - 1e-7, "not monotone at {step}");
            previous = y;
        }
    }

    #[test]
    fn the_asymmetric_shaper_is_genuinely_not_odd() {
        // Which is the whole point: an odd curve cannot make a second harmonic.
        let mismatch = (asymmetric_clip(0.8) + asymmetric_clip(-0.8)).abs();
        assert!(
            mismatch > 0.02,
            "the two halves are too similar: {mismatch}"
        );
        assert!(asymmetric_clip(4.0).abs() <= 1.0);
    }

    #[test]
    fn the_hard_clipper_is_bounded_and_transparent_below_its_knee() {
        assert_eq!(hard_clip(0.5), 0.5, "below the knee, untouched");
        assert!(hard_clip(2.0) <= 1.0);
        assert!(hard_clip(-2.0) >= -1.0);
        // Monotone: a clipper that folds back would be a wavefolder.
        let mut previous = -1.5_f32;
        let mut last = hard_clip(previous);
        while previous < 1.5 {
            previous += 0.01;
            let y = hard_clip(previous);
            assert!(y >= last - 1e-6, "not monotone at {previous}");
            last = y;
        }
    }

    #[test]
    fn an_envelope_follower_reaches_sixty_three_percent_in_its_time_constant() {
        let mut follower = EnvelopeFollower::new();
        follower.set_times(10.0, 100.0, SR);
        let frames = (0.010 * SR) as usize;
        for _ in 0..frames {
            follower.process(1.0);
        }
        assert!(
            (follower.value() - 0.632).abs() < 0.01,
            "10 ms should land on 1 - 1/e, got {}",
            follower.value()
        );

        // Release is the slower coefficient, so the fall takes ten times as long.
        for _ in 0..frames {
            follower.process(0.0);
        }
        assert!(
            follower.value() > 0.55,
            "release must not use the attack time"
        );
    }

    #[test]
    fn a_delay_line_returns_exactly_what_was_written_a_whole_number_of_samples_ago() {
        let mut line = DelayLine::new(64);
        for n in 0..32 {
            line.write(n as f32);
        }
        // Whole-sample reads are exact: Hermite at t = 0 is the sample itself.
        assert!((line.read(2.0) - 30.0).abs() < 1e-4, "{}", line.read(2.0));
        assert!((line.read(8.0) - 24.0).abs() < 1e-4);
        assert!((line.read(31.0) - 1.0).abs() < 1e-4);
        // Below the floor it clamps rather than reading the slot about to be
        // written.
        assert_eq!(line.read(0.5), line.read(DelayLine::MIN_DELAY));
    }

    #[test]
    fn a_delay_line_interpolates_between_samples_rather_than_snapping() {
        let mut line = DelayLine::new(64);
        for n in 0..32 {
            line.write(n as f32);
        }
        // A ramp is exactly what cubic interpolation reproduces.
        let half = line.read(8.5);
        assert!((half - 23.5).abs() < 1e-3, "{half}");
    }

    #[test]
    fn a_delay_line_refuses_a_read_it_cannot_serve_rather_than_panicking() {
        let mut line = DelayLine::new(16);
        for n in 0..16 {
            line.write(n as f32);
        }
        // Everything here is reachable from a tempo of 20 BPM and a webview.
        for delay in [0.0_f32, -5.0, 1e9, f32::NAN, f32::INFINITY] {
            assert!(line.read(delay).is_finite(), "read({delay}) was not finite");
        }
    }

    /// Linear interpolation, for the comparison the next test is about.
    fn linear_read(line: &DelayLine, delay: f32) -> f32 {
        let whole = delay.floor();
        let frac = delay - whole;
        line.read(whole) * (1.0 - frac) + line.read(whole + 1.0) * frac
    }

    #[test]
    fn cubic_interpolation_loses_far_less_at_the_top_octave_than_linear_would() {
        // The reason for cubic rather than linear: a modulated read must not
        // swing in and out of dullness at the modulation rate, and the size of
        // that swing *is* the loss at the worst fractional position.
        let mut line = DelayLine::new(256);
        let hz = 12_000.0_f32;
        // RMS, not peak: two reads at different sub-sample phases land on
        // different parts of the crest, and that difference is bigger than the
        // effect being measured.
        let (mut whole, mut cubic, mut linear) = (0.0_f64, 0.0_f64, 0.0_f64);
        for n in 0..8_000 {
            line.write((TAU * hz * n as f32 / SR).sin());
            if n > 512 {
                whole += f64::from(line.read(64.0)).powi(2);
                cubic += f64::from(line.read(64.5)).powi(2);
                linear += f64::from(linear_read(&line, 64.5)).powi(2);
            }
        }
        let cubic_db = 10.0 * (cubic / whole).log10();
        let linear_db = 10.0 * (linear / whole).log10();
        assert!(
            cubic_db > linear_db + 1.5,
            "cubic lost {cubic_db:.2} dB and linear {linear_db:.2} dB at 12 kHz;              the whole reason for the extra multiplies is that gap"
        );
        assert!(
            cubic_db.abs() < 1.5,
            "a half-sample cubic read lost {cubic_db} dB at 12 kHz"
        );
    }

    #[test]
    fn clearing_a_delay_line_leaves_nothing_to_come_back() {
        let mut line = DelayLine::new(32);
        for _ in 0..32 {
            line.write(1.0);
        }
        assert!(!line.is_silent());
        line.clear();
        assert!(line.is_silent());
        assert_eq!(line.read(4.0), 0.0);
    }
}

/// A whole-sample delay of fixed length.
///
/// What the saturator's dry path needs: the oversampler's latency is an integer
/// number of samples (that is why it uses a linear-phase FIR at all), so the dry
/// signal only has to be *late*, never interpolated. A [`DelayLine`] would work
/// and would cost four multiplies a sample to compute an answer it already has.
#[derive(Debug)]
pub struct FixedDelay {
    buffer: Box<[f32]>,
    write: usize,
    length: usize,
}

impl FixedDelay {
    /// A delay that can be set to anything up to `max` samples.
    ///
    /// **Non-RT**: allocates.
    pub fn new(max: usize) -> Self {
        Self {
            buffer: vec![0.0; max.max(1)].into_boxed_slice(),
            write: 0,
            length: 0,
        }
    }

    /// Set the delay, clamped to what was allocated. Clears on a change, so a
    /// switch of oversampling factor cannot smear the old latency into the new.
    pub fn set_length(&mut self, length: usize) {
        let length = length.min(self.buffer.len());
        if length != self.length {
            self.length = length;
            self.clear();
        }
    }

    /// The delay in force, in samples.
    pub const fn length(&self) -> usize {
        self.length
    }

    pub fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }

    /// Push one sample and return the one from `length` samples ago.
    ///
    /// A length of zero is a pass-through, and exactly so: the saturator with
    /// oversampling off must be able to blend a dry path that is bit-identical
    /// to its input.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        if self.length == 0 {
            return x;
        }
        let len = self.buffer.len();
        let read = (self.write + len - self.length) % len;
        let y = self.buffer[read];
        self.buffer[self.write] = x;
        self.write += 1;
        if self.write == len {
            self.write = 0;
        }
        y
    }
}
