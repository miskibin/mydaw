//! Meter frames and the rate limiter that keeps them off the webview's back.

use std::time::{Duration, Instant};

use rtrb::Consumer;
use serde::{Deserialize, Serialize};

/// Default emission rate. 30 Hz is comfortably above the ~24 Hz at which meter
/// movement reads as continuous, and well under the cost threshold for WebView2
/// IPC (the Phase 1 spike measures the real ceiling).
pub const DEFAULT_METER_HZ: f64 = 30.0;

/// One metering sample set, produced on the audio thread.
///
/// `Copy` and fixed-size: pushing it into an `rtrb` queue from the audio callback
/// allocates nothing and runs no destructor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MeterFrame {
    pub peak_l: f32,
    pub peak_r: f32,
    pub rms_l: f32,
    pub rms_r: f32,
    /// Transport position at the end of the measured block.
    pub sample_pos: u64,
    pub playing: bool,
}

impl MeterFrame {
    /// All-zero frame at position 0, stopped.
    pub const SILENT: Self = Self {
        peak_l: 0.0,
        peak_r: 0.0,
        rms_l: 0.0,
        rms_r: 0.0,
        sample_pos: 0,
        playing: false,
    };
}

/// Rate limiter between the audio thread's meter queue and the webview.
///
/// The audio thread pushes one frame per block (~344 Hz at 48 kHz / 128 frames).
/// Forwarding all of that over IPC would swamp the webview for no visual benefit,
/// so this drains the queue, keeps only the newest frame, and releases at most
/// `rate_hz` frames per second. Dropping intermediate frames is correct for
/// metering: only the latest value is displayed. Peak *hold* semantics, if wanted,
/// belong in the metering plugin on the RT side, not here.
///
/// Lives on a non-RT thread (the IPC pump).
pub struct MeterThrottle {
    rx: Consumer<MeterFrame>,
    latest: Option<MeterFrame>,
    min_interval: Duration,
    last_emit: Option<Instant>,
}

impl MeterThrottle {
    /// Throttle at [`DEFAULT_METER_HZ`].
    pub fn new(rx: Consumer<MeterFrame>) -> Self {
        Self::with_rate(rx, DEFAULT_METER_HZ)
    }

    /// Throttle at `rate_hz`. Non-finite or non-positive rates mean "no limit".
    pub fn with_rate(rx: Consumer<MeterFrame>, rate_hz: f64) -> Self {
        let min_interval = if rate_hz.is_finite() && rate_hz > 0.0 {
            Duration::from_secs_f64(1.0 / rate_hz)
        } else {
            Duration::ZERO
        };
        Self {
            rx,
            latest: None,
            min_interval,
            last_emit: None,
        }
    }

    /// Minimum spacing between emitted frames.
    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    /// Drain the queue and return the newest frame if the rate budget allows.
    ///
    /// `now` is injected rather than read from the clock so tests are
    /// deterministic; the caller passes `Instant::now()`.
    ///
    /// Returns `None` when nothing new arrived, or when the last emission was too
    /// recent -- in that case the frame is retained and offered again next call.
    pub fn poll(&mut self, now: Instant) -> Option<MeterFrame> {
        // Draining fully (not just one pop) is what keeps the audio thread's
        // producer from ever finding the queue full.
        while let Ok(frame) = self.rx.pop() {
            self.latest = Some(frame);
        }

        let due = match self.last_emit {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= self.min_interval,
        };
        if !due {
            return None;
        }

        let frame = self.latest.take()?;
        self.last_emit = Some(now);
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(peak: f32) -> MeterFrame {
        MeterFrame {
            peak_l: peak,
            peak_r: peak,
            ..MeterFrame::SILENT
        }
    }

    #[test]
    fn emits_nothing_when_queue_is_empty() {
        let (_tx, rx) = rtrb::RingBuffer::<MeterFrame>::new(8);
        let mut throttle = MeterThrottle::new(rx);
        assert!(throttle.poll(Instant::now()).is_none());
    }

    #[test]
    fn keeps_only_the_newest_frame() {
        let (mut tx, rx) = rtrb::RingBuffer::<MeterFrame>::new(8);
        let mut throttle = MeterThrottle::new(rx);
        for p in [0.1_f32, 0.2, 0.9] {
            tx.push(frame(p)).unwrap();
        }
        let out = throttle.poll(Instant::now()).unwrap();
        assert_eq!(out.peak_l, 0.9);
    }

    #[test]
    fn respects_the_configured_rate() {
        let (mut tx, rx) = rtrb::RingBuffer::<MeterFrame>::new(64);
        // 10 Hz -> one frame per 100 ms.
        let mut throttle = MeterThrottle::with_rate(rx, 10.0);
        assert_eq!(throttle.min_interval(), Duration::from_millis(100));

        let t0 = Instant::now();
        tx.push(frame(0.1)).unwrap();
        assert!(throttle.poll(t0).is_some(), "first frame emits immediately");

        // Too soon: held back, not discarded.
        tx.push(frame(0.2)).unwrap();
        assert!(throttle.poll(t0 + Duration::from_millis(50)).is_none());

        // Budget available again -> the retained newest frame comes out.
        let out = throttle.poll(t0 + Duration::from_millis(100)).unwrap();
        assert_eq!(out.peak_l, 0.2);
    }

    #[test]
    fn emits_at_most_rate_frames_per_second() {
        let (mut tx, rx) = rtrb::RingBuffer::<MeterFrame>::new(4);
        let mut throttle = MeterThrottle::with_rate(rx, 30.0);
        let t0 = Instant::now();

        // Simulate one second of audio blocks at ~1 ms each, polling every step.
        let mut emitted = 0;
        for ms in 0..1000u64 {
            let _ = tx.push(frame(0.5));
            if throttle.poll(t0 + Duration::from_millis(ms)).is_some() {
                emitted += 1;
            }
        }
        // 1000 ms / 33.33 ms = 30 slots (the first fires at t=0, so 30 or 31).
        assert!(
            (30..=31).contains(&emitted),
            "expected ~30 emissions in 1 s, got {emitted}"
        );
    }

    #[test]
    fn zero_rate_means_unthrottled() {
        let (mut tx, rx) = rtrb::RingBuffer::<MeterFrame>::new(8);
        let mut throttle = MeterThrottle::with_rate(rx, 0.0);
        let t0 = Instant::now();
        tx.push(frame(0.1)).unwrap();
        assert!(throttle.poll(t0).is_some());
        tx.push(frame(0.2)).unwrap();
        assert!(throttle.poll(t0).is_some(), "no minimum spacing");
    }

    #[test]
    fn frame_serialises_to_the_expected_json_shape() {
        let json = serde_json::to_value(MeterFrame {
            peak_l: 0.5,
            peak_r: 0.25,
            rms_l: 0.1,
            rms_r: 0.05,
            sample_pos: 1024,
            playing: true,
        })
        .unwrap();
        assert_eq!(json["peak_l"], 0.5);
        assert_eq!(json["sample_pos"], 1024);
        assert_eq!(json["playing"], true);
    }
}
