//! Where the hits are, and whether they are on the grid.
//!
//! [`analysis`](crate::analysis) answers "what is in this capture" -- levels,
//! defects, spectrum. It says nothing about *time*, and a sequencer is entirely
//! about time. This module is the missing half: it finds the onsets in a
//! capture, and measures how far each one is from where a tempo grid says it
//! should be.
//!
//! That measurement is the acceptance test for the step sequencer. "It sounds
//! tight" is not a result; "every onset is within 0.4 ms of the grid" is.
//!
//! # How the detector works
//!
//! ```text
//!   samples -> band filter -> |x| -> boxcar envelope -> rise test -> refine
//!              (optional)            (window / hop)      (ratio +      (to the
//!                                                         min gap)      sample)
//! ```
//!
//! 1. **Band.** An optional one-pole filter, applied twice. Splitting the mix is
//!    what makes the measurement trustworthy on a *drum pattern*: lowpassed, the
//!    capture contains the kick and nothing else, so counting kicks is counting
//!    onsets. A general-purpose polyphonic onset detector is a research project;
//!    a band-limited threshold is a measurement.
//! 2. **Envelope.** Mean of `|x|` over a sliding window, evaluated every
//!    [`OnsetConfig::hop`] samples. The window has to span at least one cycle of
//!    the lowest frequency in the band, or the envelope follows the carrier
//!    instead of the note.
//! 3. **Rise.** An onset fires where the envelope is both above
//!    [`OnsetConfig::on_fraction`] of the capture's own peak envelope -- so
//!    nothing has to be tuned to a level -- *and* at least
//!    [`OnsetConfig::rise_ratio`] times what it was one window earlier. A ratio
//!    rather than a level is what lets a quiet hi-hat be detected over a
//!    decaying kick: what matters is that the envelope turned upwards.
//!    [`OnsetConfig::min_gap_seconds`] then collapses the several hops of one
//!    attack into a single hit.
//! 4. **Refinement.** The rise test finds a window, not a sample. Inside it, the
//!    reported position is the first sample at which a short running mean
//!    reaches [`OnsetConfig::refine_fraction`] of the transient's own level.
//!
//! That last threshold is a *consistent* fraction of the attack rather than its
//! true beginning, so every onset carries the same small detection latency. For
//! measuring whether hits are evenly spaced -- which is the question -- a
//! constant latency cancels: [`align_grid`] fits the origin of the grid, and the
//! offset lands there instead of in the errors.
//!
//! # What it is not
//! No spectral flux, no adaptive threshold, no source separation. It is tuned
//! for percussive material in a known band against a known grid, which is
//! exactly the signal the sequencer's hardware verification produces, and it is
//! honest about the difference.

use serde::Serialize;

/// Which part of the spectrum to measure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Band {
    /// Everything, unfiltered.
    Full,
    /// Below `hz`. The kick, and nothing else in a drum mix.
    Low(f32),
    /// Above `hz`. Hats, snare noise, and the transient of anything else.
    High(f32),
}

impl Band {
    /// Apply the band to `samples`, returning the filtered signal.
    ///
    /// Two cascaded one-poles: 12 dB/octave, which is ample to separate a 50 Hz
    /// kick from a 9 kHz hi-hat and cheap enough not to need a design tool.
    /// A non-finite or non-positive corner frequency passes the signal through,
    /// because a broken filter must not be able to invent onsets.
    pub fn apply(self, samples: &[f32], sample_rate: f64) -> Vec<f32> {
        let (hz, high) = match self {
            Self::Full => return samples.to_vec(),
            Self::Low(hz) => (hz, false),
            Self::High(hz) => (hz, true),
        };
        if !hz.is_finite() || hz <= 0.0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
            return samples.to_vec();
        }

        // One-pole coefficient: `a = 1 - exp(-2*pi*fc/fs)`, the standard
        // impulse-invariant form, clamped so a corner above Nyquist is a
        // pass-through rather than an oscillator.
        let a = (1.0 - (-std::f64::consts::TAU * f64::from(hz) / sample_rate).exp()).clamp(0.0, 1.0)
            as f32;

        let mut out = Vec::with_capacity(samples.len());
        let mut stage1 = 0.0_f32;
        let mut stage2 = 0.0_f32;
        for sample in samples {
            let x = if sample.is_finite() { *sample } else { 0.0 };
            stage1 += a * (x - stage1);
            stage2 += a * (stage1 - stage2);
            out.push(if high { x - stage2 } else { stage2 });
        }
        out
    }
}

/// Tunables for [`detect_onsets`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnsetConfig {
    /// Part of the spectrum to measure.
    pub band: Band,
    /// Samples between envelope evaluations. 64 is 1.3 ms at 48 kHz.
    pub hop: usize,
    /// Samples averaged per envelope point. Must span a cycle of the lowest
    /// frequency in the band: 1024 is 21 ms, comfortably more than a 50 Hz
    /// period.
    pub window: usize,
    /// Envelope level, as a fraction of the capture's peak envelope, below
    /// which a rise is not a hit.
    pub on_fraction: f32,
    /// How much louder the envelope must be than it was one window earlier.
    pub rise_ratio: f32,
    /// Shortest interval between two onsets, in seconds. Collapses the several
    /// hops of one attack into a single hit.
    pub min_gap_seconds: f64,
    /// Samples in the short running mean the refinement uses.
    pub refine_window: usize,
    /// Fraction of the transient level at which the refinement calls it
    /// started. See the module docs on why consistency beats accuracy here.
    pub refine_fraction: f32,
}

impl Default for OnsetConfig {
    /// Tuned for a synthesised drum kit at ordinary levels, full band.
    fn default() -> Self {
        Self {
            band: Band::Full,
            hop: 64,
            window: 1_024,
            on_fraction: 0.15,
            rise_ratio: 1.8,
            min_gap_seconds: 0.02,
            refine_window: 64,
            refine_fraction: 0.25,
        }
    }
}

impl OnsetConfig {
    /// The kick's band: below `hz`, with a window long enough for it.
    #[must_use]
    pub fn low(hz: f32) -> Self {
        Self {
            band: Band::Low(hz),
            ..Self::default()
        }
    }

    /// The hats' band: above `hz`. A short window suffices up there, which is
    /// what makes a hi-hat's onset easy to place to the sample.
    #[must_use]
    pub fn high(hz: f32) -> Self {
        Self {
            band: Band::High(hz),
            window: 128,
            ..Self::default()
        }
    }
}

/// One detected hit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Onset {
    /// Sample index within the analysed channel.
    pub frame: usize,
    /// [`Self::frame`] in seconds from the start of the capture.
    pub time_seconds: f64,
    /// Peak magnitude of the transient that starts here, in the analysed band.
    pub peak: f32,
}

/// Find the onsets in one channel of samples.
///
/// Returns them in time order. An empty slice, a nonsense sample rate or a
/// degenerate configuration yields no onsets rather than an error: this is a
/// measurement, and "nothing detectable" is a legitimate measurement.
pub fn detect_onsets(samples: &[f32], sample_rate: f64, config: &OnsetConfig) -> Vec<Onset> {
    if config.hop == 0
        || config.window == 0
        || samples.len() <= config.window
        || !(sample_rate.is_finite() && sample_rate > 0.0)
    {
        return Vec::new();
    }

    let signal = config.band.apply(samples, sample_rate);
    let points = (signal.len() - config.window) / config.hop + 1;
    let envelope: Vec<f32> = (0..points)
        .map(|point| {
            let start = point * config.hop;
            let sum: f64 = signal[start..start + config.window]
                .iter()
                .map(|s| f64::from(s.abs()))
                .sum();
            (sum / config.window as f64) as f32
        })
        .collect();

    let peak_envelope = envelope.iter().fold(0.0_f32, |peak, e| peak.max(*e));
    if peak_envelope <= 0.0 {
        return Vec::new();
    }
    let floor = peak_envelope * config.on_fraction;
    let min_gap = (config.min_gap_seconds * sample_rate).max(0.0) as usize;
    // One window back, so the comparison is against a window that does not
    // overlap this one -- otherwise a long attack compares against itself.
    let lag = (config.window / config.hop).max(1);

    let mut onsets: Vec<Onset> = Vec::new();
    for (point, level) in envelope.iter().enumerate() {
        if *level < floor {
            continue;
        }
        // Before the first full lag there is nothing to compare against, and
        // the honest reading of signal where there was no capture is a rise
        // from silence. `min_gap` collapses the run of points that produces.
        let previous = if point >= lag {
            envelope[point - lag]
        } else {
            0.0
        };
        if previous > 0.0 && *level < previous * config.rise_ratio {
            continue;
        }

        let crossing = point * config.hop;
        let frame = refine(&signal, crossing, config);
        let peak_end = (frame + config.window * 2).min(signal.len());
        let peak = signal[frame..peak_end]
            .iter()
            .fold(0.0_f32, |peak, s| peak.max(s.abs()));

        if let Some(previous_onset) = onsets.last() {
            if frame.saturating_sub(previous_onset.frame) < min_gap {
                continue;
            }
        }
        onsets.push(Onset {
            frame,
            time_seconds: frame as f64 / sample_rate,
            peak,
        });
    }

    onsets
}

/// Place the onset to the sample, inside the window that triggered it.
///
/// The rise test fires on the first envelope window that contains enough of the
/// transient, so the transient itself lies in `[crossing - hop, crossing +
/// window)`. Inside that region a short running mean of `|x|` is computed, and
/// the onset is the first sample at which it reaches
/// [`OnsetConfig::refine_fraction`] of the peak of that region.
fn refine(signal: &[f32], crossing: usize, config: &OnsetConfig) -> usize {
    let fine = config.refine_window.max(1);
    let start = crossing.saturating_sub(config.hop);
    let end = (crossing + config.window + fine).min(signal.len());
    if end < start + fine + 1 {
        return crossing;
    }

    // Running mean of |x| over `fine` samples, one entry per start position.
    // The divisor is constant, so it is left out: only ratios are used.
    let last = end - fine;
    let mut running: f64 = signal[start..start + fine]
        .iter()
        .map(|s| f64::from(s.abs()))
        .sum();
    let mut means = Vec::with_capacity(last - start + 1);
    means.push(running);
    for position in (start + 1)..=last {
        running += f64::from(signal[position + fine - 1].abs());
        running -= f64::from(signal[position - 1].abs());
        means.push(running);
    }

    let peak = means.iter().fold(0.0_f64, |peak, m| peak.max(*m));
    if peak <= 0.0 {
        return crossing;
    }
    let threshold = peak * f64::from(config.refine_fraction);
    match means.iter().position(|m| *m >= threshold) {
        Some(index) => start + index,
        None => crossing,
    }
}

/// How far one onset sits from its nearest grid line.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridDeviation {
    /// Index of the nearest grid line, counted from the grid's origin.
    pub line: i64,
    /// Onset position in samples.
    pub frame: usize,
    /// Onset minus grid line, in samples. Positive means late.
    pub error_samples: f64,
    /// The same error in milliseconds.
    pub error_ms: f64,
}

/// The alignment of a whole capture against a tempo grid.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridReport {
    /// Grid spacing in samples (a step, a beat -- whatever was measured).
    pub step_samples: f64,
    /// Where line 0 of the grid sits in the capture. See [`align_grid`].
    pub origin_samples: f64,
    pub onset_count: usize,
    /// Largest absolute error, in milliseconds.
    pub max_abs_error_ms: f64,
    /// Mean absolute error, in milliseconds.
    pub mean_abs_error_ms: f64,
    /// Largest absolute error, in samples.
    pub max_abs_error_samples: f64,
    pub deviations: Vec<GridDeviation>,
}

impl GridReport {
    /// Whether every onset is within `tolerance_ms` of a grid line.
    pub fn within(&self, tolerance_ms: f64) -> bool {
        self.max_abs_error_ms <= tolerance_ms
    }

    /// Whether the onsets fall on consecutive grid lines with none missing.
    ///
    /// A pattern that hits every line should produce this; a gap means the
    /// detector missed a hit (or the engine did), and the two look identical
    /// from a mean error alone.
    pub fn is_contiguous(&self) -> bool {
        self.deviations
            .windows(2)
            .all(|pair| pair[1].line == pair[0].line + 1)
    }
}

/// Measure `onsets` against a grid of `step_samples` starting at
/// `origin_samples`.
///
/// Each onset is assigned to its *nearest* grid line, so a missing hit costs a
/// line rather than shifting every later measurement by one -- which is what
/// makes the report readable when something is missing.
pub fn grid_report(
    onsets: &[Onset],
    step_samples: f64,
    origin_samples: f64,
    sample_rate: f64,
) -> GridReport {
    let mut deviations = Vec::with_capacity(onsets.len());
    if step_samples.is_finite()
        && step_samples > 0.0
        && sample_rate.is_finite()
        && sample_rate > 0.0
    {
        for onset in onsets {
            let relative = onset.frame as f64 - origin_samples;
            let line = (relative / step_samples).round();
            let error_samples = relative - line * step_samples;
            deviations.push(GridDeviation {
                line: line as i64,
                frame: onset.frame,
                error_samples,
                error_ms: error_samples / sample_rate * 1_000.0,
            });
        }
    }

    let max_abs_error_samples = deviations
        .iter()
        .fold(0.0_f64, |worst, d| worst.max(d.error_samples.abs()));
    let max_abs_error_ms = deviations
        .iter()
        .fold(0.0_f64, |worst, d| worst.max(d.error_ms.abs()));
    let mean_abs_error_ms = if deviations.is_empty() {
        0.0
    } else {
        deviations.iter().map(|d| d.error_ms.abs()).sum::<f64>() / deviations.len() as f64
    };

    GridReport {
        step_samples,
        origin_samples,
        onset_count: onsets.len(),
        max_abs_error_ms,
        mean_abs_error_ms,
        max_abs_error_samples,
        deviations,
    }
}

/// The grid origin that best explains `onsets`, and the report for it.
///
/// A capture does not begin at the transport's sample 0: the tap is armed some
/// blocks after Play, so the first hit lands at an unknown offset. Rather than
/// requiring the caller to know that offset, this finds the origin that
/// minimises the worst error -- which is the honest way to ask "are these hits
/// evenly spaced on this grid" without also asking "did the capture start on a
/// bar line".
///
/// Candidates are the onsets themselves (a grid that fits at all passes near
/// one), each refined once by the mean signed error it produces.
pub fn align_grid(onsets: &[Onset], step_samples: f64, sample_rate: f64) -> GridReport {
    if onsets.is_empty() || !(step_samples.is_finite() && step_samples > 0.0) {
        return grid_report(onsets, step_samples, 0.0, sample_rate);
    }

    let mut best = grid_report(onsets, step_samples, onsets[0].frame as f64, sample_rate);
    for candidate in onsets {
        let origin = candidate.frame as f64;
        let report = grid_report(onsets, step_samples, origin, sample_rate);
        let mean_error: f64 = report
            .deviations
            .iter()
            .map(|d| d.error_samples)
            .sum::<f64>()
            / report.deviations.len() as f64;
        let refined = grid_report(onsets, step_samples, origin + mean_error, sample_rate);
        for report in [report, refined] {
            if report.max_abs_error_samples < best.max_abs_error_samples {
                best = report;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    /// A decaying sine burst added into `samples` at `frame`.
    fn hit(samples: &mut [f32], frame: usize, amplitude: f32, hz: f64, decay_seconds: f64) {
        for index in 0..samples.len().saturating_sub(frame) {
            let t = index as f64 / SR;
            let envelope = (-t / decay_seconds).exp();
            let value = (std::f64::consts::TAU * hz * t).sin() * envelope;
            samples[frame + index] += (value as f32) * amplitude;
        }
    }

    /// Silence with a 60 Hz "kick" at each of `at`.
    fn kicks(frames: usize, at: &[usize]) -> Vec<f32> {
        let mut samples = vec![0.0_f32; frames];
        for frame in at {
            hit(&mut samples, *frame, 0.6, 60.0, 0.08);
        }
        samples
    }

    #[test]
    fn onsets_are_found_within_a_few_samples_of_where_they_were_put() {
        let at = [0_usize, 12_000, 24_000, 36_000];
        let samples = kicks(60_000, &at);
        let onsets = detect_onsets(&samples, SR, &OnsetConfig::default());

        assert_eq!(onsets.len(), at.len(), "{onsets:?}");
        for (found, expected) in onsets.iter().zip(at) {
            let error = found.frame as i64 - expected as i64;
            assert!(
                error.abs() <= 32,
                "onset at {} should be {expected} (error {error} samples)",
                found.frame
            );
            assert!(found.peak > 0.3);
            assert!((found.time_seconds - found.frame as f64 / SR).abs() < 1e-12);
        }
    }

    #[test]
    fn a_low_carrier_does_not_make_the_envelope_chatter() {
        // The failure this guards against: a window shorter than one cycle of
        // the carrier follows the *waveform*, and every peak of a 60 Hz sine
        // reads as a new onset.
        let samples = kicks(60_000, &[0, 24_000]);
        let onsets = detect_onsets(&samples, SR, &OnsetConfig::default());
        assert_eq!(
            onsets.len(),
            2,
            "one per hit, not one per cycle: {onsets:?}"
        );
    }

    #[test]
    fn a_band_separates_a_kick_from_a_hat_so_each_can_be_counted() {
        // The whole reason `Band` exists. A drum mix, measured twice.
        let mut samples = vec![0.0_f32; 96_000];
        for frame in [0_usize, 24_000, 48_000, 72_000] {
            hit(&mut samples, frame, 0.6, 60.0, 0.08);
        }
        for frame in (0..96_000).step_by(12_000) {
            hit(&mut samples, frame, 0.12, 9_000.0, 0.01);
        }

        // The kick band: four hits, evenly spaced. Their absolute positions
        // carry the filter's group delay (a 120 Hz two-pole is a couple of
        // milliseconds), which is why the assertion is about *spacing* -- and
        // why `align_grid` fits an origin rather than assuming one.
        let low = detect_onsets(&samples, SR, &OnsetConfig::low(120.0));
        assert_eq!(low.len(), 4, "four kicks: {low:?}");
        let grid = align_grid(&low, 24_000.0, SR);
        assert!(
            grid.within(0.2),
            "kick spacing was off by {} ms: {low:?}",
            grid.max_abs_error_ms
        );
        assert!(grid.is_contiguous());

        // The hat band: eight hits, and up here there is no group delay worth
        // the name, so they land where they were put.
        let high = detect_onsets(&samples, SR, &OnsetConfig::high(4_000.0));
        assert_eq!(high.len(), 8, "eight hats: {high:?}");
        for (index, found) in high.iter().enumerate() {
            let expected = (index * 12_000) as i64;
            assert!(
                (found.frame as i64 - expected).abs() <= 48,
                "hat {index} at {} should be {expected}",
                found.frame
            );
        }
        assert!(align_grid(&high, 12_000.0, SR).within(1.0));
    }

    #[test]
    fn silence_and_nonsense_inputs_yield_no_onsets_rather_than_an_error() {
        let config = OnsetConfig::default();
        assert!(detect_onsets(&[], SR, &config).is_empty());
        assert!(detect_onsets(&vec![0.0; 48_000], SR, &config).is_empty());
        assert!(detect_onsets(&kicks(48_000, &[0]), 0.0, &config).is_empty());
        assert!(detect_onsets(&kicks(48_000, &[0]), f64::NAN, &config).is_empty());
        for broken in [
            OnsetConfig { hop: 0, ..config },
            OnsetConfig {
                window: 0,
                ..config
            },
        ] {
            assert!(detect_onsets(&kicks(48_000, &[0]), SR, &broken).is_empty());
        }
    }

    #[test]
    fn a_steady_tone_is_one_onset_not_a_thousand() {
        // A signal that comes up and stays up rises once: after the attack the
        // envelope equals what it was a window ago, so the ratio test is false
        // for every point after it.
        let mut samples = vec![0.0_f32; 96_000];
        for (index, sample) in samples.iter_mut().enumerate().skip(4_800) {
            *sample = ((std::f64::consts::TAU * 440.0 * index as f64 / SR).sin() * 0.5) as f32;
        }
        let onsets = detect_onsets(&samples, SR, &OnsetConfig::default());
        assert_eq!(onsets.len(), 1, "{onsets:?}");
        assert!((onsets[0].frame as i64 - 4_800).abs() <= 64);
    }

    #[test]
    fn a_broken_band_passes_the_signal_through_instead_of_inventing_onsets() {
        let samples = kicks(60_000, &[0, 24_000]);
        for hz in [0.0_f32, -100.0, f32::NAN, f32::INFINITY] {
            let filtered = Band::Low(hz).apply(&samples, SR);
            assert_eq!(filtered, samples);
            assert_eq!(Band::High(hz).apply(&samples, SR), samples);
        }
        assert_eq!(Band::Full.apply(&samples, SR), samples);
        assert_eq!(Band::Low(120.0).apply(&samples, 0.0), samples);
    }

    #[test]
    fn a_perfect_grid_reports_zero_error() {
        let step = 6_000.0;
        let onsets: Vec<Onset> = (0..8)
            .map(|line| Onset {
                frame: (line as f64 * step) as usize,
                time_seconds: 0.0,
                peak: 1.0,
            })
            .collect();
        let report = grid_report(&onsets, step, 0.0, SR);
        assert_eq!(report.onset_count, 8);
        assert_eq!(report.max_abs_error_samples, 0.0);
        assert!(report.within(0.001));
        assert!(report.is_contiguous());
        assert_eq!(
            report.deviations.iter().map(|d| d.line).collect::<Vec<_>>(),
            (0..8).collect::<Vec<i64>>()
        );
    }

    #[test]
    fn a_known_error_is_reported_in_samples_and_milliseconds() {
        let step = 4_800.0; // 100 ms at 48 kHz
        let onsets = vec![
            Onset {
                frame: 0,
                time_seconds: 0.0,
                peak: 1.0,
            },
            // 48 samples late: exactly 1 ms.
            Onset {
                frame: 4_848,
                time_seconds: 0.0,
                peak: 1.0,
            },
        ];
        let report = grid_report(&onsets, step, 0.0, SR);
        assert_eq!(report.deviations[1].error_samples, 48.0);
        assert!((report.deviations[1].error_ms - 1.0).abs() < 1e-9);
        assert!((report.max_abs_error_ms - 1.0).abs() < 1e-9);
        assert!((report.mean_abs_error_ms - 0.5).abs() < 1e-9);
        assert!(report.within(1.0));
        assert!(!report.within(0.9));
    }

    #[test]
    fn alignment_finds_the_grid_when_the_capture_did_not_start_on_one() {
        let step = 5_538.46_f64;
        let offset = 1_234.0;
        let onsets: Vec<Onset> = (0..12)
            .map(|line| Onset {
                frame: (offset + line as f64 * step).round() as usize,
                time_seconds: 0.0,
                peak: 1.0,
            })
            .collect();

        let unaligned = grid_report(&onsets, step, 0.0, SR);
        assert!(
            unaligned.max_abs_error_samples > 100.0,
            "the wrong origin should look wrong"
        );

        let aligned = align_grid(&onsets, step, SR);
        assert!(
            aligned.max_abs_error_samples <= 1.0,
            "aligned worst error was {} samples",
            aligned.max_abs_error_samples
        );
        assert!(aligned.within(0.05));
        assert!(aligned.is_contiguous());
    }

    #[test]
    fn a_missing_hit_costs_a_grid_line_rather_than_shifting_everything_after_it() {
        let step = 6_000.0;
        // Lines 0, 1, 3, 4 -- line 2 never sounded.
        let onsets: Vec<Onset> = [0_usize, 6_000, 18_000, 24_000]
            .into_iter()
            .map(|frame| Onset {
                frame,
                time_seconds: 0.0,
                peak: 1.0,
            })
            .collect();
        let report = grid_report(&onsets, step, 0.0, SR);
        assert_eq!(
            report.deviations.iter().map(|d| d.line).collect::<Vec<_>>(),
            vec![0, 1, 3, 4]
        );
        assert_eq!(report.max_abs_error_samples, 0.0);
        assert!(!report.is_contiguous(), "the gap has to be visible");
    }

    #[test]
    fn an_empty_or_impossible_grid_degrades_instead_of_dividing_by_zero() {
        for step in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let report = grid_report(&[], step, 0.0, SR);
            assert_eq!(report.onset_count, 0);
            assert!(report.deviations.is_empty());
            assert_eq!(report.max_abs_error_ms, 0.0);
            assert!(align_grid(&[], step, SR).deviations.is_empty());
        }
    }

    #[test]
    fn the_report_serialises_with_the_key_names_an_agent_reads() {
        let report = grid_report(
            &[Onset {
                frame: 10,
                time_seconds: 0.0,
                peak: 1.0,
            }],
            6_000.0,
            0.0,
            SR,
        );
        let json = serde_json::to_value(&report).unwrap();
        for key in [
            "stepSamples",
            "originSamples",
            "onsetCount",
            "maxAbsErrorMs",
            "meanAbsErrorMs",
            "maxAbsErrorSamples",
            "deviations",
        ] {
            assert!(json.get(key).is_some(), "missing {key} in {json}");
        }
        assert!(json["deviations"][0]["errorSamples"].is_number());
        assert!(json["deviations"][0]["frame"].is_number());
    }
}
