//! [`Capture`] -- a block of audio the engine actually played -- and
//! [`DebugTapService`], the non-RT API that produces one.
//!
//! ```text
//!   caller (IPC command, headless main, test)
//!        |
//!        v
//!   DebugTapService::capture(duration, timeout)
//!        | arm ------------------------------> TapShared::armed = true
//!        | drain ... sleep 1 ms ... drain ...   (audio thread pushes blocks)
//!        | disarm ----------------------------> TapShared::armed = false
//!        v
//!   Capture { samples, sample_rate, channels: 2, dropped }
//!        |-> stats()        -> CaptureStats (serde)
//!        |-> write_wav()    -> 32-bit float stereo WAV
//!        |-> render_png()   -> waveform + spectrum PNG (+ axis sidecar)
//!        '-> summary_text() -> compact report for a human or an agent
//! ```
//!
//! Nothing in this module runs on the audio thread. The only contact with it is
//! through the atomics and the ring in [`crate::tap`].

use std::path::Path;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::analysis::{
    channel_stats, spectrum_with_planner, CaptureStats, Spectrum, CHANNEL_LABELS,
};
use crate::error::CaptureIoError;
use crate::render::{render_png, RenderedPng};
use crate::tap::{TapConsumer, TapShared, CHANNELS};
use crate::wav::write_wav;

/// How often the capture loop looks at the ring while waiting.
///
/// One millisecond is far shorter than any sane audio block (a 128-frame block
/// at 48 kHz is 2.7 ms) and far longer than the cost of a `slots()` load, so the
/// loop neither spins nor misses a wake-up.
pub const DRAIN_INTERVAL: Duration = Duration::from_millis(1);

/// Longest capture the service will attempt, in seconds.
///
/// The buffer is pre-allocated (`duration * sample_rate * 2` floats), so this
/// bound is what stops a fat-fingered `debug_capture(1e9)` from asking for a
/// terabyte. 120 s at 48 kHz stereo is 46 MB, which is a lot but not a crash.
pub const MAX_CAPTURE_SECONDS: f64 = 120.0;

/// Why a capture did not produce a full buffer.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// Another capture is already draining the ring. Captures are serialised by
    /// construction: there is one consumer, and it is not shareable.
    #[error("a capture is already in progress")]
    Busy,

    /// No audio stream has ever run, so the tap does not know the sample rate.
    /// On a machine with no output device this is the normal answer.
    #[error("the audio engine has not started; no sample rate is known")]
    EngineNotRunning,

    /// A duration of zero (or one that rounds to zero frames).
    #[error("capture duration must be greater than zero")]
    ZeroDuration,

    /// A duration beyond [`MAX_CAPTURE_SECONDS`].
    #[error("capture duration {requested:.3} s exceeds the {max:.0} s limit")]
    TooLong { requested: f64, max: f64 },

    /// The timeout expired before `want` frames arrived. Carries everything that
    /// *did* arrive -- a partial capture is usually the most useful diagnostic
    /// there is, because the common cause is "the transport was not playing".
    #[error("timed out after {got} of {want} frames")]
    TimedOutWithSamples {
        got: usize,
        want: usize,
        capture: Box<Capture>,
    },
}

impl CaptureError {
    /// The partial capture carried by [`Self::TimedOutWithSamples`], if any.
    pub fn partial(&self) -> Option<&Capture> {
        match self {
            Self::TimedOutWithSamples { capture, .. } => Some(capture),
            _ => None,
        }
    }

    /// Consume the error and take the partial capture out of it.
    pub fn into_partial(self) -> Option<Capture> {
        match self {
            Self::TimedOutWithSamples { capture, .. } => Some(*capture),
            _ => None,
        }
    }
}

/// A stretch of the engine's output bus, exactly as the graph produced it.
#[derive(Clone, PartialEq)]
pub struct Capture {
    /// Interleaved `L R L R ...`. Always a whole number of frames.
    pub samples: Vec<f32>,
    pub sample_rate: f64,
    /// Always [`CHANNELS`]; carried explicitly so serialised captures stay
    /// self-describing when the engine grows past stereo.
    pub channels: usize,
    /// Samples the audio thread had to throw away during this capture.
    pub dropped: u64,
}

/// Deliberately hand-written: the derived `Debug` would print every sample, and
/// these values end up in panic messages and error logs.
impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("frames", &self.frames())
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("dropped", &self.dropped)
            .finish()
    }
}

impl Capture {
    /// Wrap interleaved samples. Any trailing partial frame is discarded.
    pub fn new(mut samples: Vec<f32>, sample_rate: f64, dropped: u64) -> Self {
        let whole = samples.len() - samples.len() % CHANNELS;
        samples.truncate(whole);
        Self {
            samples,
            sample_rate,
            channels: CHANNELS,
            dropped,
        }
    }

    /// Frames captured.
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    /// Whether nothing at all was captured.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Captured length in seconds.
    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate > 0.0 {
            self.frames() as f64 / self.sample_rate
        } else {
            0.0
        }
    }

    /// One channel, de-interleaved into a fresh `Vec`.
    ///
    /// Out-of-range indices give an empty vector rather than panicking: this is
    /// reachable from an IPC command, and a debug tool that crashes the DAW is
    /// worse than no debug tool.
    pub fn channel(&self, channel: usize) -> Vec<f32> {
        if channel >= self.channels {
            return Vec::new();
        }
        self.samples
            .iter()
            .skip(channel)
            .step_by(self.channels)
            .copied()
            .collect()
    }

    /// Measure everything. See [`crate::analysis`] for the definitions.
    pub fn stats(&self) -> CaptureStats {
        let mut planner = realfft::RealFftPlanner::<f64>::new();
        let mut per_channel = Vec::with_capacity(self.channels);
        let mut fft_size = 0;
        let mut resolution = 0.0;

        for channel in 0..self.channels {
            let samples = self.channel(channel);
            let spectrum: Spectrum =
                spectrum_with_planner(&samples, self.sample_rate, &mut planner);
            if spectrum.size > 0 {
                fft_size = spectrum.size;
                resolution = spectrum.bin_hz;
            }
            let label = CHANNEL_LABELS
                .get(channel)
                .copied()
                .unwrap_or("?")
                .to_owned();
            per_channel.push(channel_stats(&label, &samples, self.sample_rate, &spectrum));
        }

        CaptureStats {
            sample_rate: self.sample_rate,
            channels: self.channels,
            frames: self.frames(),
            duration_seconds: self.duration_seconds(),
            dropped_samples: self.dropped,
            fft_size,
            frequency_resolution_hz: resolution,
            window: "hann",
            per_channel,
        }
    }

    /// Write a 32-bit float stereo WAV. Creates parent directories.
    pub fn write_wav(&self, path: impl AsRef<Path>) -> Result<(), CaptureIoError> {
        write_wav(self, path.as_ref())
    }

    /// Render the waveform/spectrum inspection image plus its axis sidecar.
    pub fn render_png(&self, path: impl AsRef<Path>) -> Result<RenderedPng, CaptureIoError> {
        render_png(self, path.as_ref())
    }

    /// Peak amplitude per column, across all channels. Used by the sparkline and
    /// (in spirit) by the PNG renderer.
    pub fn envelope(&self, columns: usize) -> Vec<f32> {
        let frames = self.frames();
        if columns == 0 || frames == 0 {
            return Vec::new();
        }
        (0..columns)
            .map(|column| {
                let start = column * frames / columns;
                let end = (((column + 1) * frames / columns).max(start + 1)).min(frames);
                let mut peak = 0.0_f32;
                for frame in start..end {
                    for channel in 0..self.channels {
                        let sample = self.samples[frame * self.channels + channel];
                        if sample.is_finite() {
                            peak = peak.max(sample.abs());
                        }
                    }
                }
                peak
            })
            .collect()
    }

    /// An ASCII sparkline of the capture's envelope, on a -60..0 dBFS scale.
    ///
    /// Pure ASCII on purpose: this ends up in a Windows console whose code page
    /// is anyone's guess, and a row of replacement characters is not a
    /// diagnostic.
    pub fn sparkline(&self, columns: usize) -> String {
        const RAMP: &[u8] = b" .:-=+*#%@";
        let envelope = self.envelope(columns);
        if envelope.is_empty() {
            return String::new();
        }
        envelope
            .iter()
            .map(|peak| {
                let db = crate::analysis::dbfs(*peak);
                let level = if db.is_finite() {
                    (((db + 60.0) / 60.0).clamp(0.0, 1.0) * (RAMP.len() - 1) as f32).round()
                        as usize
                } else {
                    0
                };
                RAMP[level.min(RAMP.len() - 1)] as char
            })
            .collect()
    }

    /// A compact report meant to be read by a person or pasted into an agent's
    /// context. Deterministic, ASCII, no colour codes.
    pub fn summary_text(&self) -> String {
        let stats = self.stats();
        let mut out = String::new();
        out.push_str(&format!(
            "MYDAW capture: {} frames, {:.3} s, {} ch @ {:.0} Hz, dropped {} samples\n",
            stats.frames,
            stats.duration_seconds,
            stats.channels,
            stats.sample_rate,
            stats.dropped_samples,
        ));
        out.push_str(&format!(
            "analysis: {} window, fft {} bins, resolution {:.2} Hz\n",
            stats.window, stats.fft_size, stats.frequency_resolution_hz,
        ));

        for channel in &stats.per_channel {
            out.push_str(&format!(
                "  {}: peak {} ({:+.4})  rms {}  dc {:+.6}  clipped {}  non-finite {}\n",
                channel.label,
                format_db(channel.peak_dbfs),
                channel.peak,
                format_db(channel.rms_dbfs),
                channel.dc_offset,
                channel.clipped,
                channel.non_finite,
            ));
            out.push_str(&format!(
                "     dominant {}  zero-cross {:.1} Hz  flatness {:.4}\n",
                match (channel.dominant_hz, channel.dominant_magnitude_dbfs) {
                    (Some(hz), Some(db)) => format!("{hz:.1} Hz at {}", format_db(db)),
                    _ => "none".to_owned(),
                },
                channel.zero_crossing_hz,
                channel.spectral_flatness,
            ));
            if !channel.top_bins.is_empty() {
                let peaks: Vec<String> = channel
                    .top_bins
                    .iter()
                    .map(|peak| {
                        format!(
                            "{:.0}Hz/{}",
                            peak.frequency_hz,
                            format_db(peak.magnitude_dbfs)
                        )
                    })
                    .collect();
                out.push_str(&format!("     peaks {}\n", peaks.join("  ")));
            }
        }

        let sparkline = self.sparkline(72);
        if !sparkline.is_empty() {
            out.push_str(&format!(
                "  env [{sparkline}]  (space = -60 dBFS, @ = 0 dBFS)\n"
            ));
        }
        if stats.dropped_samples > 0 {
            out.push_str("  WARNING: samples were dropped; this capture has holes\n");
        }
        if stats.has_non_finite() {
            out.push_str("  WARNING: the graph produced NaN or infinite samples\n");
        }
        if stats.has_clipping() {
            out.push_str("  WARNING: samples at or beyond full scale\n");
        }
        out
    }
}

/// Render a dBFS value for humans, spelling out digital silence.
pub fn format_db(db: f32) -> String {
    if db.is_finite() {
        format!("{db:.2} dBFS")
    } else {
        "-inf dBFS".to_owned()
    }
}

/// The non-RT face of the debug tap.
///
/// Provided as a kernel service by [`crate::DebugTapPlugin`]; resolve it with
/// `ctx.services.get::<DebugTapService>()`.
///
/// Concurrency: the consumer lives behind a `Mutex` that is `try_lock`ed, so a
/// second simultaneous capture gets [`CaptureError::Busy`] instead of two
/// readers on a single-consumer queue.
pub struct DebugTapService {
    consumer: Mutex<TapConsumer>,
    shared: std::sync::Arc<TapShared>,
}

impl DebugTapService {
    /// Wrap the consumer half of a [`crate::debug_tap_channel`].
    pub fn new(consumer: TapConsumer) -> Self {
        let shared = std::sync::Arc::clone(consumer.shared());
        Self {
            consumer: Mutex::new(consumer),
            shared,
        }
    }

    /// Sample rate the audio thread last reported, or 0 when no stream has run.
    pub fn sample_rate(&self) -> f64 {
        self.shared.sample_rate()
    }

    /// Whether a capture is currently armed.
    pub fn is_armed(&self) -> bool {
        self.shared.is_armed()
    }

    /// Samples dropped during the most recent capture.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped()
    }

    /// Shared flags, for wiring and tests.
    pub fn shared(&self) -> &std::sync::Arc<TapShared> {
        &self.shared
    }

    /// Collect `duration` of audio, giving up after `timeout`.
    ///
    /// Arms the tap, drains the ring until enough frames have arrived, then
    /// disarms it -- on every exit path, including the timeout.
    ///
    /// # Errors
    /// - [`CaptureError::EngineNotRunning`] when no stream has ever opened.
    /// - [`CaptureError::ZeroDuration`] / [`CaptureError::TooLong`] for a
    ///   duration outside `(0, MAX_CAPTURE_SECONDS]`.
    /// - [`CaptureError::Busy`] if another capture holds the consumer.
    /// - [`CaptureError::TimedOutWithSamples`] if the buffer did not fill in
    ///   time. **This is the normal result when the transport is stopped**, and
    ///   it carries whatever was collected.
    pub fn capture(&self, duration: Duration, timeout: Duration) -> Result<Capture, CaptureError> {
        let sample_rate = self.shared.sample_rate();
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(CaptureError::EngineNotRunning);
        }

        let seconds = duration.as_secs_f64();
        if seconds > MAX_CAPTURE_SECONDS {
            return Err(CaptureError::TooLong {
                requested: seconds,
                max: MAX_CAPTURE_SECONDS,
            });
        }
        let want_frames = (seconds * sample_rate).round() as usize;
        if want_frames == 0 {
            return Err(CaptureError::ZeroDuration);
        }
        let want_samples = want_frames * CHANNELS;

        let mut consumer = self.consumer.try_lock().ok_or(CaptureError::Busy)?;

        // Whatever is in the ring predates this capture.
        consumer.drain_all();
        self.shared.reset_dropped();

        let mut samples: Vec<f32> = Vec::with_capacity(want_samples);
        let deadline = Instant::now() + timeout;
        self.shared.arm();

        let mut timed_out = false;
        loop {
            consumer.drain_into(&mut samples, want_samples);
            if samples.len() >= want_samples {
                break;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
            std::thread::sleep(DRAIN_INTERVAL);
        }

        self.shared.disarm();
        // A block may have landed between the last drain and the disarm.
        consumer.drain_into(&mut samples, want_samples);
        drop(consumer);

        samples.truncate(want_samples);
        let capture = Capture::new(samples, sample_rate, self.shared.dropped());

        if timed_out && capture.frames() < want_frames {
            return Err(CaptureError::TimedOutWithSamples {
                got: capture.frames(),
                want: want_frames,
                capture: Box::new(capture),
            });
        }
        Ok(capture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tap::debug_tap_channel;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const SR: f64 = 48_000.0;

    fn sine_pair(frequency: f64, amplitude: f32, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let left: Vec<f32> = (0..frames)
            .map(|i| {
                let phase = std::f64::consts::TAU * frequency * i as f64 / SR;
                (phase.sin() as f32) * amplitude
            })
            .collect();
        let right = left.clone();
        (left, right)
    }

    fn capture_of(frequency: f64, amplitude: f32, frames: usize) -> Capture {
        let (left, right) = sine_pair(frequency, amplitude, frames);
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            samples.push(left[i]);
            samples.push(right[i]);
        }
        Capture::new(samples, SR, 0)
    }

    #[test]
    fn a_capture_reports_its_own_shape() {
        let capture = capture_of(440.0, 0.5, 4_800);
        assert_eq!(capture.channels, 2);
        assert_eq!(capture.frames(), 4_800);
        assert!((capture.duration_seconds() - 0.1).abs() < 1e-9);
        assert!(!capture.is_empty());
    }

    #[test]
    fn a_trailing_half_frame_is_discarded_rather_than_misread() {
        let capture = Capture::new(vec![1.0, 2.0, 3.0], SR, 0);
        assert_eq!(capture.frames(), 1);
        assert_eq!(capture.samples, vec![1.0, 2.0]);
    }

    #[test]
    fn channels_deinterleave_and_out_of_range_is_empty_not_a_panic() {
        let capture = Capture::new(vec![1.0, -1.0, 2.0, -2.0], SR, 0);
        assert_eq!(capture.channel(0), vec![1.0, 2.0]);
        assert_eq!(capture.channel(1), vec![-1.0, -2.0]);
        assert!(capture.channel(2).is_empty());
        assert!(capture.channel(usize::MAX).is_empty());
    }

    #[test]
    fn stats_measure_both_channels_of_a_real_capture() {
        let capture = capture_of(440.0, 0.5, 24_000);
        let stats = capture.stats();
        assert_eq!(stats.per_channel.len(), 2);
        assert_eq!(stats.per_channel[0].label, "L");
        assert_eq!(stats.per_channel[1].label, "R");
        assert_eq!(stats.fft_size, 16_384);
        for channel in &stats.per_channel {
            assert!((channel.peak_dbfs - (-6.02)).abs() < 0.02);
            assert!((channel.dominant_hz.unwrap() - 440.0).abs() < 1.0);
        }
    }

    #[test]
    fn the_sparkline_is_ascii_and_tracks_the_envelope() {
        let capture = capture_of(440.0, 0.5, 24_000);
        let line = capture.sparkline(40);
        assert_eq!(line.chars().count(), 40);
        assert!(line.is_ascii());
        assert!(
            line.chars().all(|c| c != ' '),
            "a steady tone should not read as silence: {line}"
        );

        let silence = Capture::new(vec![0.0; 4_800], SR, 0);
        assert_eq!(silence.sparkline(10), " ".repeat(10));
        assert!(Capture::new(Vec::new(), SR, 0).sparkline(10).is_empty());
        assert!(capture.sparkline(0).is_empty());
    }

    #[test]
    fn the_summary_names_the_numbers_an_agent_needs() {
        let text = capture_of(440.0, 0.5, 24_000).summary_text();
        assert!(text.contains("24000 frames"), "{text}");
        assert!(text.contains("-6.02 dBFS"), "{text}");
        assert!(text.contains("440"), "{text}");
        assert!(text.contains("hann"), "{text}");
        assert!(text.is_ascii(), "the summary must survive any code page");
    }

    #[test]
    fn the_summary_warns_about_defects() {
        let mut samples = vec![0.5_f32; 4_096];
        samples[0] = f32::NAN;
        samples[2] = 2.0;
        let text = Capture::new(samples, SR, 17).summary_text();
        assert!(text.contains("dropped 17 samples"), "{text}");
        assert!(text.contains("WARNING: samples were dropped"), "{text}");
        assert!(text.contains("NaN or infinite"), "{text}");
        assert!(text.contains("full scale"), "{text}");
    }

    #[test]
    fn a_silent_capture_summarises_as_minus_infinity_without_panicking() {
        let text = Capture::new(vec![0.0; 9_600], SR, 0).summary_text();
        assert!(text.contains("-inf dBFS"), "{text}");
    }

    #[test]
    fn an_empty_capture_still_produces_a_summary() {
        let text = Capture::new(Vec::new(), SR, 0).summary_text();
        assert!(text.contains("0 frames"), "{text}");
    }

    #[test]
    fn capturing_before_any_stream_has_run_is_a_typed_error() {
        let (_rt, consumer) = debug_tap_channel(1_024);
        let service = DebugTapService::new(consumer);
        assert!(matches!(
            service.capture(Duration::from_millis(10), Duration::from_millis(10)),
            Err(CaptureError::EngineNotRunning)
        ));
    }

    #[test]
    fn a_zero_or_absurd_duration_is_rejected_before_anything_is_allocated() {
        let (rt, consumer) = debug_tap_channel(1_024);
        rt.set_sample_rate(SR);
        let service = DebugTapService::new(consumer);

        assert!(matches!(
            service.capture(Duration::ZERO, Duration::from_millis(10)),
            Err(CaptureError::ZeroDuration)
        ));
        assert!(matches!(
            service.capture(Duration::from_secs(3_600), Duration::from_millis(10)),
            Err(CaptureError::TooLong { .. })
        ));
        assert!(
            !service.is_armed(),
            "a rejected capture must not arm the tap"
        );
    }

    #[test]
    fn a_capture_with_nothing_feeding_it_times_out_and_returns_what_it_has() {
        let (rt, consumer) = debug_tap_channel(1_024);
        rt.set_sample_rate(SR);
        let service = DebugTapService::new(consumer);

        let error = service
            .capture(Duration::from_millis(200), Duration::from_millis(60))
            .expect_err("nothing is producing audio");

        match error {
            CaptureError::TimedOutWithSamples { got, want, capture } => {
                assert_eq!(got, 0);
                assert_eq!(want, (0.2 * SR) as usize);
                assert!(capture.is_empty());
                assert_eq!(capture.sample_rate, SR);
            }
            other => panic!("expected a timeout, got {other}"),
        }
        assert!(
            !service.is_armed(),
            "the tap must be disarmed on timeout too"
        );
    }

    #[test]
    fn a_timeout_carries_the_partial_audio_that_did_arrive() {
        let (mut rt, consumer) = debug_tap_channel(4_096);
        rt.set_sample_rate(SR);
        let service = Arc::new(DebugTapService::new(consumer));

        let feeder_service = Arc::clone(&service);
        let stop = Arc::new(AtomicBool::new(false));
        let feeder_stop = Arc::clone(&stop);
        let feeder = std::thread::spawn(move || {
            let (left, right) = sine_pair(440.0, 0.5, 128);
            // Feed only 1 000 frames, then stall: the capture wants far more.
            let mut pushed = 0;
            while !feeder_stop.load(Ordering::Relaxed) {
                if pushed < 1_000 && feeder_service.is_armed() {
                    rt.push_block(&left, &right);
                    pushed += 128;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let error = service
            .capture(Duration::from_millis(500), Duration::from_millis(400))
            .expect_err("the feeder stops short of the requested duration");
        stop.store(true, Ordering::Relaxed);
        feeder.join().unwrap();

        let partial = error.partial().expect("a timeout carries its samples");
        assert!(
            partial.frames() > 0 && partial.frames() < 24_000,
            "expected a partial capture, got {} frames",
            partial.frames()
        );
        assert!(partial.stats().per_channel[0].peak > 0.0);
    }

    #[test]
    fn a_capture_that_is_fed_enough_audio_succeeds_and_measures_it() {
        let (mut rt, consumer) = debug_tap_channel(1 << 16);
        rt.set_sample_rate(SR);
        let service = Arc::new(DebugTapService::new(consumer));

        let feeder_service = Arc::clone(&service);
        let stop = Arc::new(AtomicBool::new(false));
        let feeder_stop = Arc::clone(&stop);
        let feeder = std::thread::spawn(move || {
            let (left, right) = sine_pair(440.0, 0.2, 256);
            while !feeder_stop.load(Ordering::Relaxed) {
                if feeder_service.is_armed() {
                    rt.push_block(&left, &right);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let capture = service
            .capture(Duration::from_millis(100), Duration::from_secs(5))
            .expect("the feeder supplies more than enough");
        stop.store(true, Ordering::Relaxed);
        feeder.join().unwrap();

        assert_eq!(capture.frames(), 4_800);
        assert_eq!(capture.channels, 2);
        let stats = capture.stats();
        assert!(
            (stats.per_channel[0].peak - 0.2).abs() < 0.01,
            "peak was {}",
            stats.per_channel[0].peak
        );
        assert!(!service.is_armed());
    }

    #[test]
    fn a_second_simultaneous_capture_is_refused_rather_than_racing() {
        let (rt, consumer) = debug_tap_channel(1_024);
        rt.set_sample_rate(SR);
        let service = Arc::new(DebugTapService::new(consumer));

        let background = Arc::clone(&service);
        let handle = std::thread::spawn(move || {
            background.capture(Duration::from_millis(200), Duration::from_millis(250))
        });

        // Wait until the background capture has taken the consumer lock.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !service.is_armed() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }

        let second = service.capture(Duration::from_millis(10), Duration::from_millis(10));
        assert!(
            matches!(second, Err(CaptureError::Busy)),
            "expected Busy, got {second:?}"
        );
        let _ = handle.join().unwrap();
    }

    #[test]
    fn stale_samples_from_before_the_capture_are_discarded() {
        let (mut rt, consumer) = debug_tap_channel(4_096);
        rt.set_sample_rate(SR);
        let service = Arc::new(DebugTapService::new(consumer));

        // Arm by hand, push junk, disarm: the ring now holds stale audio.
        service.shared().arm();
        rt.push_block(&[9.0; 64], &[9.0; 64]);
        service.shared().disarm();

        let feeder_service = Arc::clone(&service);
        let stop = Arc::new(AtomicBool::new(false));
        let feeder_stop = Arc::clone(&stop);
        let feeder = std::thread::spawn(move || {
            while !feeder_stop.load(Ordering::Relaxed) {
                if feeder_service.is_armed() {
                    rt.push_block(&[0.25; 128], &[0.25; 128]);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let capture = service
            .capture(Duration::from_millis(20), Duration::from_secs(5))
            .expect("the feeder supplies enough");
        stop.store(true, Ordering::Relaxed);
        feeder.join().unwrap();

        assert!(
            capture.samples.iter().all(|s| (*s - 0.25).abs() < 1e-6),
            "stale 9.0 samples leaked into the capture"
        );
    }
}
