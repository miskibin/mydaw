//! Measurement of a [`Capture`](crate::Capture): levels, defects and spectrum.
//!
//! Everything here is a pure function of a slice of samples. No audio thread, no
//! I/O, no global state -- which is what makes it testable against synthetic
//! signals whose correct answers are known in closed form (see the tests at the
//! bottom of this file).
//!
//! # dBFS and infinity
//! Digital silence is -inf dBFS, and that is what [`dbfs`] returns
//! ([`f32::NEG_INFINITY`]). `serde_json` renders a non-finite float as `null`,
//! so a silent capture serialises as `"peakDbfs": null` rather than failing or
//! inventing a floor. Consumers must handle `null`; the tests pin that
//! behaviour so it cannot drift.
//!
//! # Spectral analysis
//! The FFT is a real-input FFT (`realfft`) over the largest power of two that
//! fits in the capture, capped at [`MAX_FFT_SIZE`], with a **Hann** window.
//!
//! - Frequency resolution is `sample_rate / fft_size` and is reported in
//!   [`CaptureStats::frequency_resolution_hz`]. At 48 kHz with the default cap
//!   that is 48000/32768 = 1.46 Hz; for a 0.5 s capture (24 000 frames, so
//!   `fft_size` = 16 384) it is 2.93 Hz.
//! - Bin magnitudes are scaled to **amplitude**: a full-scale sine reads 1.0
//!   (0 dBFS) regardless of the FFT size. The factor is `2 / sum(window)`, which
//!   for Hann is `4 / n`.
//! - Peak frequencies are refined by parabolic interpolation over the three
//!   log-magnitude bins around the maximum. That is the standard companion to a
//!   Hann window and pushes the error well below one bin, which is why a 440 Hz
//!   tone reads as 440 Hz and not "somewhere in bin 150".

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde::Serialize;

use crate::tap::CHANNELS;

/// Largest FFT the analysis will run. 32 768 points is ~0.68 s at 48 kHz and
/// takes well under a millisecond; larger windows would smear a capture that is
/// not stationary anyway.
pub const MAX_FFT_SIZE: usize = 1 << 15;

/// Shortest capture worth transforming. Below this the spectrum is omitted
/// rather than reported at a resolution of several hundred hertz.
pub const MIN_FFT_SIZE: usize = 1 << 6;

/// Per-channel labels, in interleave order.
pub const CHANNEL_LABELS: [&str; CHANNELS] = ["L", "R"];

/// How many spectral peaks [`ChannelStats::top_bins`] reports.
pub const TOP_BIN_COUNT: usize = 5;

/// Amplitude at or above which a sample counts as clipped.
pub const CLIP_THRESHOLD: f32 = 1.0;

/// Convert a linear amplitude to dBFS.
///
/// Zero, negative and non-finite inputs return [`f32::NEG_INFINITY`] -- there is
/// no such thing as a negative amplitude in a magnitude measurement, and a NaN
/// sample must not be able to produce a NaN level.
pub fn dbfs(linear: f32) -> f32 {
    if linear.is_finite() && linear > 0.0 {
        20.0 * linear.log10()
    } else {
        f32::NEG_INFINITY
    }
}

/// A Hann window of `n` points, `0.5 - 0.5*cos(2*pi*i/n)` (periodic form).
///
/// The periodic (not symmetric) form is the correct one for spectral analysis:
/// it makes the window's own spectrum land exactly on bin centres.
pub fn hann_window(n: usize) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos())
        .collect()
}

/// One interpolated spectral peak.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpectralPeak {
    pub frequency_hz: f64,
    /// Peak amplitude in dBFS (`null` in JSON when the bin is empty).
    pub magnitude_dbfs: f32,
}

/// A magnitude spectrum in amplitude units, one entry per FFT bin.
#[derive(Debug, Clone)]
pub struct Spectrum {
    /// Number of time-domain points transformed. 0 when the capture was too
    /// short to analyse.
    pub size: usize,
    /// Hz per bin (`sample_rate / size`).
    pub bin_hz: f64,
    /// `size / 2 + 1` amplitudes; index 0 is DC.
    pub magnitudes: Vec<f32>,
}

impl Spectrum {
    /// An empty spectrum, for captures below [`MIN_FFT_SIZE`].
    pub fn empty() -> Self {
        Self {
            size: 0,
            bin_hz: 0.0,
            magnitudes: Vec::new(),
        }
    }

    /// Whether anything was transformed.
    pub fn is_empty(&self) -> bool {
        self.magnitudes.is_empty()
    }

    /// Centre frequency of bin `k`.
    pub fn frequency_of(&self, bin: usize) -> f64 {
        bin as f64 * self.bin_hz
    }

    /// Highest frequency represented (Nyquist).
    pub fn nyquist_hz(&self) -> f64 {
        if self.magnitudes.is_empty() {
            0.0
        } else {
            self.frequency_of(self.magnitudes.len() - 1)
        }
    }

    /// The loudest bin above DC, refined by parabolic interpolation.
    ///
    /// Returns `None` for an empty spectrum or one with no energy at all.
    pub fn dominant(&self) -> Option<SpectralPeak> {
        let peak_bin = self.peak_bin()?;
        Some(self.interpolate(peak_bin))
    }

    /// Up to `count` interpolated local maxima, loudest first.
    ///
    /// Local maxima rather than "the N largest bins": the N largest bins of a
    /// sine are the N bins of its single main lobe, which tells you nothing.
    pub fn top_peaks(&self, count: usize) -> Vec<SpectralPeak> {
        if self.magnitudes.len() < 3 || count == 0 {
            return Vec::new();
        }
        let mut maxima: Vec<usize> = (1..self.magnitudes.len() - 1)
            .filter(|&k| {
                self.magnitudes[k] > 0.0
                    && self.magnitudes[k] > self.magnitudes[k - 1]
                    && self.magnitudes[k] >= self.magnitudes[k + 1]
            })
            .collect();
        maxima.sort_by(|a, b| {
            self.magnitudes[*b]
                .partial_cmp(&self.magnitudes[*a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        maxima
            .into_iter()
            .take(count)
            .map(|k| self.interpolate(k))
            .collect()
    }

    /// Spectral flatness (Wiener entropy): geometric mean over arithmetic mean of
    /// the power spectrum, excluding DC.
    ///
    /// 1.0 is white noise, values near 0 are tonal. A spectrum with no energy
    /// returns 0.0 rather than NaN -- silence has no timbre, and a NaN would
    /// serialise as `null` and poison downstream comparisons.
    pub fn flatness(&self) -> f32 {
        if self.magnitudes.len() < 2 {
            return 0.0;
        }
        const EPSILON: f64 = 1e-20;
        let bins = &self.magnitudes[1..];
        let mut log_sum = 0.0_f64;
        let mut sum = 0.0_f64;
        for magnitude in bins {
            let power = (*magnitude as f64) * (*magnitude as f64);
            log_sum += (power + EPSILON).ln();
            sum += power;
        }
        if sum <= 0.0 {
            return 0.0;
        }
        let n = bins.len() as f64;
        let geometric = (log_sum / n).exp();
        let arithmetic = sum / n;
        ((geometric / arithmetic) as f32).clamp(0.0, 1.0)
    }

    fn peak_bin(&self) -> Option<usize> {
        if self.magnitudes.len() < 2 {
            return None;
        }
        let mut best = 1;
        let mut best_value = self.magnitudes[1];
        for (k, value) in self.magnitudes.iter().enumerate().skip(2) {
            if *value > best_value {
                best_value = *value;
                best = k;
            }
        }
        (best_value > 0.0).then_some(best)
    }

    /// Parabolic interpolation over the log-magnitudes at `bin-1, bin, bin+1`.
    fn interpolate(&self, bin: usize) -> SpectralPeak {
        let magnitude = self.magnitudes[bin];
        if bin == 0 || bin + 1 >= self.magnitudes.len() {
            return SpectralPeak {
                frequency_hz: self.frequency_of(bin),
                magnitude_dbfs: dbfs(magnitude),
            };
        }
        let left = dbfs(self.magnitudes[bin - 1]) as f64;
        let centre = dbfs(magnitude) as f64;
        let right = dbfs(self.magnitudes[bin + 1]) as f64;

        // A neighbour at -inf (an exactly-zero bin) makes the parabola
        // meaningless; fall back to the bin centre.
        if !left.is_finite() || !centre.is_finite() || !right.is_finite() {
            return SpectralPeak {
                frequency_hz: self.frequency_of(bin),
                magnitude_dbfs: dbfs(magnitude),
            };
        }

        let denominator = left - 2.0 * centre + right;
        let delta = if denominator.abs() > f64::EPSILON {
            (0.5 * (left - right) / denominator).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let refined_db = centre - 0.25 * (left - right) * delta;
        SpectralPeak {
            frequency_hz: (bin as f64 + delta) * self.bin_hz,
            magnitude_dbfs: refined_db as f32,
        }
    }
}

/// Hann-windowed magnitude spectrum of `samples`.
///
/// Non-finite samples are replaced by zero before transforming: one NaN would
/// otherwise turn every bin into NaN and destroy the whole measurement. The
/// count of those samples is reported separately in
/// [`ChannelStats::non_finite`].
pub fn spectrum(samples: &[f32], sample_rate: f64) -> Spectrum {
    let mut planner = RealFftPlanner::<f64>::new();
    spectrum_with_planner(samples, sample_rate, &mut planner)
}

/// [`spectrum`], reusing a planner across channels.
pub fn spectrum_with_planner(
    samples: &[f32],
    sample_rate: f64,
    planner: &mut RealFftPlanner<f64>,
) -> Spectrum {
    let size = fft_size_for(samples.len());
    if size == 0 || !(sample_rate.is_finite() && sample_rate > 0.0) {
        return Spectrum::empty();
    }

    let window = hann_window(size);
    let mut input: Vec<f64> = samples[..size]
        .iter()
        .zip(&window)
        .map(|(sample, w)| {
            let value = *sample as f64;
            if value.is_finite() {
                value * w
            } else {
                0.0
            }
        })
        .collect();

    let fft: Arc<dyn RealToComplex<f64>> = planner.plan_fft_forward(size);
    let mut output: Vec<Complex<f64>> = fft.make_output_vec();
    if fft.process(&mut input, &mut output).is_err() {
        return Spectrum::empty();
    }

    // Amplitude scaling: 2/sum(w) turns a windowed sinusoid of amplitude A into
    // a bin magnitude of A. DC has no mirrored half, so it uses 1/sum(w).
    let window_sum: f64 = window.iter().sum();
    let scale = if window_sum > 0.0 {
        2.0 / window_sum
    } else {
        0.0
    };
    let magnitudes: Vec<f32> = output
        .iter()
        .enumerate()
        .map(|(k, bin)| {
            let factor = if k == 0 { scale * 0.5 } else { scale };
            (bin.norm() * factor) as f32
        })
        .collect();

    Spectrum {
        size,
        bin_hz: sample_rate / size as f64,
        magnitudes,
    }
}

/// Largest usable power-of-two FFT size for `frames` samples, or 0 if too short.
pub fn fft_size_for(frames: usize) -> usize {
    if frames < MIN_FFT_SIZE {
        return 0;
    }
    let capped = frames.min(MAX_FFT_SIZE);
    // Largest power of two <= capped.
    1usize << (usize::BITS - 1 - capped.leading_zeros()) as usize
}

/// Level and defect measurements for one channel.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStats {
    /// `"L"` or `"R"`.
    pub label: String,
    /// Largest absolute sample, linear.
    pub peak: f32,
    /// [`Self::peak`] in dBFS; `null` in JSON for digital silence.
    pub peak_dbfs: f32,
    /// Root mean square over the finite samples, linear.
    pub rms: f32,
    /// [`Self::rms`] in dBFS; `null` in JSON for digital silence.
    pub rms_dbfs: f32,
    /// Mean sample value. A non-zero DC offset wastes headroom and thumps on
    /// playback start.
    pub dc_offset: f32,
    /// Samples that are NaN or infinite. Any non-zero value is a DSP bug.
    pub non_finite: usize,
    /// Samples with `|s| >= 1.0`.
    pub clipped: usize,
    /// Fundamental estimated from DC-corrected zero crossings. Cheap, robust for
    /// a single tone, meaningless for a chord.
    pub zero_crossing_hz: f64,
    /// Loudest spectral component above DC; `null` in JSON when there is none.
    pub dominant_hz: Option<f64>,
    /// Amplitude of [`Self::dominant_hz`] in dBFS.
    pub dominant_magnitude_dbfs: Option<f32>,
    /// Wiener entropy of the spectrum: ~1 for noise, ~0 for a pure tone.
    pub spectral_flatness: f32,
    /// The [`TOP_BIN_COUNT`] loudest spectral peaks, loudest first.
    pub top_bins: Vec<SpectralPeak>,
}

/// The full report for a capture. Serialises camelCase for the webview and for
/// the headless stdout dump.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStats {
    pub sample_rate: f64,
    pub channels: usize,
    pub frames: usize,
    pub duration_seconds: f64,
    /// Samples the audio thread dropped because the ring was full. Non-zero
    /// means the capture has holes and the timings in it are not trustworthy.
    pub dropped_samples: u64,
    /// FFT length used, or 0 when the capture was too short to transform.
    pub fft_size: usize,
    /// `sample_rate / fft_size`, or 0 when there is no spectrum.
    pub frequency_resolution_hz: f64,
    /// Analysis window name, for the record.
    pub window: &'static str,
    pub per_channel: Vec<ChannelStats>,
}

impl CaptureStats {
    /// Whether any channel contains a NaN or an infinity.
    pub fn has_non_finite(&self) -> bool {
        self.per_channel.iter().any(|c| c.non_finite > 0)
    }

    /// Whether any channel contains a sample at or beyond full scale.
    pub fn has_clipping(&self) -> bool {
        self.per_channel.iter().any(|c| c.clipped > 0)
    }
}

/// Measure one channel's worth of samples.
pub fn channel_stats(
    label: &str,
    samples: &[f32],
    sample_rate: f64,
    spectrum: &Spectrum,
) -> ChannelStats {
    let mut peak = 0.0_f32;
    let mut sum = 0.0_f64;
    let mut sum_squares = 0.0_f64;
    let mut finite = 0_usize;
    let mut non_finite = 0_usize;
    let mut clipped = 0_usize;

    for sample in samples {
        if !sample.is_finite() {
            non_finite += 1;
            continue;
        }
        finite += 1;
        let magnitude = sample.abs();
        if magnitude > peak {
            peak = magnitude;
        }
        if magnitude >= CLIP_THRESHOLD {
            clipped += 1;
        }
        sum += *sample as f64;
        sum_squares += (*sample as f64) * (*sample as f64);
    }

    let dc_offset = if finite > 0 {
        (sum / finite as f64) as f32
    } else {
        0.0
    };
    let rms = if finite > 0 {
        (sum_squares / finite as f64).sqrt() as f32
    } else {
        0.0
    };

    let dominant = spectrum.dominant();
    ChannelStats {
        label: label.to_owned(),
        peak,
        peak_dbfs: dbfs(peak),
        rms,
        rms_dbfs: dbfs(rms),
        dc_offset,
        non_finite,
        clipped,
        zero_crossing_hz: zero_crossing_frequency(samples, sample_rate, dc_offset),
        dominant_hz: dominant.map(|p| p.frequency_hz),
        dominant_magnitude_dbfs: dominant.map(|p| p.magnitude_dbfs),
        spectral_flatness: spectrum.flatness(),
        top_bins: spectrum.top_peaks(TOP_BIN_COUNT),
    }
}

/// Fundamental frequency estimated from sign changes of the DC-corrected signal.
///
/// Two crossings per cycle, so `crossings / 2 / seconds`. Non-finite samples and
/// exact zeros are skipped rather than counted as a crossing, which keeps a
/// silent-then-loud capture from reading as a very high frequency.
pub fn zero_crossing_frequency(samples: &[f32], sample_rate: f64, dc_offset: f32) -> f64 {
    if samples.len() < 2 || !(sample_rate.is_finite() && sample_rate > 0.0) {
        return 0.0;
    }
    let mut crossings = 0_u64;
    let mut previous: Option<bool> = None;
    for sample in samples {
        if !sample.is_finite() {
            continue;
        }
        let centred = *sample - dc_offset;
        if centred == 0.0 {
            continue;
        }
        let positive = centred > 0.0;
        if let Some(was_positive) = previous {
            if was_positive != positive {
                crossings += 1;
            }
        }
        previous = Some(positive);
    }
    let seconds = samples.len() as f64 / sample_rate;
    if seconds <= 0.0 {
        return 0.0;
    }
    crossings as f64 / 2.0 / seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    /// A sine of `frequency` Hz at `amplitude`, `frames` long.
    fn sine(frequency: f64, amplitude: f32, frames: usize, sample_rate: f64) -> Vec<f32> {
        (0..frames)
            .map(|i| {
                let phase = std::f64::consts::TAU * frequency * i as f64 / sample_rate;
                (phase.sin() as f32) * amplitude
            })
            .collect()
    }

    fn stats_of(samples: &[f32]) -> ChannelStats {
        let spectrum = spectrum(samples, SR);
        channel_stats("L", samples, SR, &spectrum)
    }

    #[test]
    fn a_half_scale_sine_reads_minus_six_peak_and_minus_nine_rms() {
        let samples = sine(440.0, 0.5, 24_000, SR);
        let stats = stats_of(&samples);

        // 20*log10(0.5) = -6.0206, 20*log10(0.5/sqrt(2)) = -9.0309.
        assert!(
            (stats.peak_dbfs - (-6.0206)).abs() < 0.01,
            "peak was {} dBFS",
            stats.peak_dbfs
        );
        assert!(
            (stats.rms_dbfs - (-9.0309)).abs() < 0.01,
            "rms was {} dBFS",
            stats.rms_dbfs
        );
        assert_eq!(stats.clipped, 0);
        assert_eq!(stats.non_finite, 0);
        assert!(stats.dc_offset.abs() < 1e-3, "dc was {}", stats.dc_offset);
    }

    #[test]
    fn the_dominant_frequency_of_a_440_hz_tone_is_440_hz() {
        let samples = sine(440.0, 0.5, 24_000, SR);
        let stats = stats_of(&samples);
        let dominant = stats.dominant_hz.expect("a tone has a dominant bin");
        assert!(
            (dominant - 440.0).abs() < 1.0,
            "dominant was {dominant} Hz, bin width is {} Hz",
            SR / 16_384.0
        );
        let magnitude = stats.dominant_magnitude_dbfs.unwrap();
        assert!(
            (magnitude - (-6.02)).abs() < 0.5,
            "the peak bin should carry the tone's amplitude, got {magnitude} dBFS"
        );
    }

    #[test]
    fn the_zero_crossing_estimate_agrees_with_the_fft_on_a_pure_tone() {
        let samples = sine(440.0, 0.5, 24_000, SR);
        let stats = stats_of(&samples);
        assert!(
            (stats.zero_crossing_hz - 440.0).abs() < 2.0,
            "zero crossings said {} Hz",
            stats.zero_crossing_hz
        );
    }

    #[test]
    fn other_frequencies_are_found_just_as_precisely() {
        for frequency in [100.0, 1_000.0, 4_410.0, 12_000.0] {
            let samples = sine(frequency, 0.25, 24_000, SR);
            let dominant = stats_of(&samples).dominant_hz.unwrap();
            assert!(
                (dominant - frequency).abs() < 1.0,
                "{frequency} Hz read as {dominant} Hz"
            );
        }
    }

    #[test]
    fn a_dc_offset_is_measured_and_does_not_break_the_pitch_estimate() {
        let mut samples = sine(440.0, 0.25, 24_000, SR);
        for sample in &mut samples {
            *sample += 0.3;
        }
        let stats = stats_of(&samples);
        assert!(
            (stats.dc_offset - 0.3).abs() < 1e-3,
            "dc was {}",
            stats.dc_offset
        );
        assert!((stats.peak - 0.55).abs() < 1e-3, "peak was {}", stats.peak);
        assert!(
            (stats.zero_crossing_hz - 440.0).abs() < 2.0,
            "the estimate corrects for DC; got {}",
            stats.zero_crossing_hz
        );
    }

    #[test]
    fn pure_dc_has_no_zero_crossings_at_all() {
        let samples = vec![0.4_f32; 4_096];
        let stats = stats_of(&samples);
        assert_eq!(stats.zero_crossing_hz, 0.0);
        assert!((stats.dc_offset - 0.4).abs() < 1e-6);
        assert!((stats.rms - 0.4).abs() < 1e-6);
    }

    #[test]
    fn nan_and_infinity_are_counted_and_never_propagate() {
        let mut samples = sine(440.0, 0.5, 4_096, SR);
        samples[10] = f32::NAN;
        samples[20] = f32::INFINITY;
        samples[30] = f32::NEG_INFINITY;

        let stats = stats_of(&samples);
        assert_eq!(stats.non_finite, 3);
        assert!(stats.peak.is_finite() && stats.peak <= 0.5 + 1e-6);
        assert!(stats.rms.is_finite());
        assert!(stats.dc_offset.is_finite());
        assert!(stats.peak_dbfs.is_finite());
        assert!(
            stats.dominant_hz.is_some_and(|f| f.is_finite()),
            "one NaN must not destroy the spectrum"
        );
    }

    #[test]
    fn an_all_nan_channel_degrades_to_silence_rather_than_panicking() {
        let samples = vec![f32::NAN; 1_024];
        let stats = stats_of(&samples);
        assert_eq!(stats.non_finite, 1_024);
        assert_eq!(stats.peak, 0.0);
        assert_eq!(stats.rms, 0.0);
        assert_eq!(stats.dc_offset, 0.0);
        assert_eq!(stats.peak_dbfs, f32::NEG_INFINITY);
        assert_eq!(stats.zero_crossing_hz, 0.0);
    }

    #[test]
    fn clipping_is_counted_at_and_beyond_full_scale() {
        let samples = vec![0.9_f32, 1.0, -1.0, 1.5, -0.99, f32::NAN];
        let stats = stats_of(&samples);
        assert_eq!(stats.clipped, 3, "|s| >= 1.0 counts, 0.99 does not");
        assert_eq!(stats.non_finite, 1);
        assert!((stats.peak - 1.5).abs() < 1e-6);
    }

    #[test]
    fn digital_silence_is_negative_infinity_and_serialises_as_null() {
        let samples = vec![0.0_f32; 8_192];
        let stats = stats_of(&samples);
        assert_eq!(stats.peak, 0.0);
        assert_eq!(stats.peak_dbfs, f32::NEG_INFINITY);
        assert_eq!(stats.rms_dbfs, f32::NEG_INFINITY);
        assert_eq!(stats.spectral_flatness, 0.0, "no energy, no timbre");
        assert!(stats.dominant_hz.is_none());
        assert!(stats.top_bins.is_empty());

        let json: serde_json::Value = serde_json::to_value(&stats).unwrap();
        assert!(
            json["peakDbfs"].is_null(),
            "-inf dBFS must render as null, got {}",
            json["peakDbfs"]
        );
        assert!(json["rmsDbfs"].is_null());
        assert!(json["dominantHz"].is_null());
    }

    #[test]
    fn dbfs_rejects_every_kind_of_nonsense_input() {
        assert!((dbfs(1.0) - 0.0).abs() < 1e-6);
        assert!((dbfs(0.5) - (-6.0206)).abs() < 1e-3);
        assert_eq!(dbfs(0.0), f32::NEG_INFINITY);
        assert_eq!(dbfs(-0.5), f32::NEG_INFINITY);
        assert_eq!(dbfs(f32::NAN), f32::NEG_INFINITY);
        assert_eq!(dbfs(f32::INFINITY), f32::NEG_INFINITY);
    }

    #[test]
    fn the_hann_window_is_periodic_and_sums_to_half_its_length() {
        let window = hann_window(1_024);
        assert_eq!(window.len(), 1_024);
        assert!(window[0].abs() < 1e-12, "the periodic form starts at 0");
        assert!((window[512] - 1.0).abs() < 1e-12, "and peaks at the middle");
        let sum: f64 = window.iter().sum();
        assert!(
            (sum - 512.0).abs() < 1e-6,
            "sum(hann) = n/2, got {sum}; the amplitude scaling depends on it"
        );
        assert!(hann_window(0).is_empty());
    }

    #[test]
    fn the_fft_size_is_the_largest_power_of_two_that_fits() {
        assert_eq!(fft_size_for(0), 0);
        assert_eq!(fft_size_for(1), 0);
        assert_eq!(fft_size_for(MIN_FFT_SIZE - 1), 0);
        assert_eq!(fft_size_for(MIN_FFT_SIZE), MIN_FFT_SIZE);
        assert_eq!(fft_size_for(1_000), 512);
        assert_eq!(fft_size_for(24_000), 16_384);
        assert_eq!(fft_size_for(1_000_000), MAX_FFT_SIZE);
    }

    #[test]
    fn the_frequency_resolution_is_sample_rate_over_fft_size() {
        let samples = sine(440.0, 0.5, 24_000, SR);
        let spectrum = spectrum(&samples, SR);
        assert_eq!(spectrum.size, 16_384);
        assert!((spectrum.bin_hz - SR / 16_384.0).abs() < 1e-9);
        assert_eq!(spectrum.magnitudes.len(), 16_384 / 2 + 1);
        assert!((spectrum.nyquist_hz() - SR / 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_capture_too_short_to_transform_has_no_spectrum_instead_of_a_bad_one() {
        let spectrum = spectrum(&sine(440.0, 0.5, 32, SR), SR);
        assert!(spectrum.is_empty());
        assert_eq!(spectrum.size, 0);
        assert!(spectrum.dominant().is_none());
        assert_eq!(spectrum.flatness(), 0.0);
        assert!(spectrum.top_peaks(5).is_empty());
    }

    #[test]
    fn a_nonsense_sample_rate_yields_no_spectrum_rather_than_nan_bins() {
        for rate in [0.0, -48_000.0, f64::NAN, f64::INFINITY] {
            assert!(spectrum(&sine(440.0, 0.5, 4_096, SR), rate).is_empty());
        }
    }

    #[test]
    fn a_full_scale_sine_reads_zero_dbfs_in_the_spectrum_at_any_fft_size() {
        for frames in [4_096_usize, 24_000, 70_000] {
            let samples = sine(1_000.0, 1.0, frames, SR);
            let peak = spectrum(&samples, SR).dominant().unwrap();
            assert!(
                peak.magnitude_dbfs.abs() < 0.3,
                "{frames} frames gave {} dBFS; amplitude scaling is FFT-size dependent",
                peak.magnitude_dbfs
            );
        }
    }

    #[test]
    fn top_peaks_finds_separate_partials_not_one_lobe_five_times() {
        let frames = 24_000;
        let mut samples = sine(440.0, 0.5, frames, SR);
        for (i, extra) in sine(1_320.0, 0.25, frames, SR).iter().enumerate() {
            samples[i] += extra;
        }
        let spectrum = spectrum(&samples, SR);
        let peaks = spectrum.top_peaks(TOP_BIN_COUNT);
        assert!(peaks.len() >= 2, "expected both partials, got {peaks:?}");
        assert!((peaks[0].frequency_hz - 440.0).abs() < 2.0);
        assert!(
            peaks[1..]
                .iter()
                .any(|p| (p.frequency_hz - 1_320.0).abs() < 2.0),
            "the third harmonic should appear: {peaks:?}"
        );
    }

    #[test]
    fn flatness_separates_a_tone_from_noise() {
        let tone = spectrum(&sine(1_000.0, 0.5, 24_000, SR), SR).flatness();

        // Deterministic pseudo-noise: a 32-bit LCG, so the test cannot flake.
        let mut seed = 0x1234_5678_u32;
        let noise: Vec<f32> = (0..24_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / 8_388_608.0 - 1.0
            })
            .collect();
        let noise_flatness = spectrum(&noise, SR).flatness();

        assert!(tone < 0.01, "a pure tone is not flat: {tone}");
        assert!(
            noise_flatness > 0.2,
            "white noise should be much flatter: {noise_flatness}"
        );
        assert!(tone < noise_flatness);
    }

    #[test]
    fn stats_serialise_with_the_key_names_the_ui_and_agents_read() {
        let stats = stats_of(&sine(440.0, 0.5, 4_096, SR));
        let json: serde_json::Value = serde_json::to_value(&stats).unwrap();
        for key in [
            "label",
            "peak",
            "peakDbfs",
            "rms",
            "rmsDbfs",
            "dcOffset",
            "nonFinite",
            "clipped",
            "zeroCrossingHz",
            "dominantHz",
            "dominantMagnitudeDbfs",
            "spectralFlatness",
            "topBins",
        ] {
            assert!(json.get(key).is_some(), "missing key {key} in {json}");
        }
        assert!(json["topBins"][0]["frequencyHz"].is_number());
    }
}
