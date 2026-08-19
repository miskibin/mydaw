//! Meter frames and the rate limiter that keeps them off the webview's back.

use std::time::{Duration, Instant};

use mydaw_rt_core::{BusPeaks, TrackPeaks, MAX_BUSES, MAX_TRACKS};
use rtrb::Consumer;
use serde::{Deserialize, Serialize};

/// Default emission rate. 30 Hz is comfortably above the ~24 Hz at which meter
/// movement reads as continuous, and well under the cost threshold for WebView2
/// IPC (the Phase 1 spike measures the real ceiling).
pub const DEFAULT_METER_HZ: f64 = 30.0;

/// One track's level for a single block.
///
/// Measured **after** the track's strip and **before** the master fader, which
/// is what a channel meter in a mixer shows: pull a fader down and its own meter
/// follows; pull the master down and it does not.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct TrackMeter {
    pub peak_l: f32,
    pub peak_r: f32,
}

impl TrackMeter {
    /// A silent block.
    pub const SILENT: Self = Self {
        peak_l: 0.0,
        peak_r: 0.0,
    };
}

/// One metering sample set, produced on the audio thread.
///
/// `Copy` and fixed-size: pushing it into an `rtrb` queue from the audio callback
/// allocates nothing and runs no destructor. The per-track array is fixed at
/// [`MAX_TRACKS`] for the same reason -- a `Vec` here would mean an allocation
/// per block, and the DTO layer is where this becomes a list of exactly the
/// tracks that exist.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MeterFrame {
    /// Master bus peak, post master fader.
    pub peak_l: f32,
    pub peak_r: f32,
    pub rms_l: f32,
    pub rms_r: f32,
    /// Transport position at the end of the measured block.
    pub sample_pos: u64,
    pub playing: bool,
    /// Per-track peaks. Only the first [`track_count`](Self::track_count) are
    /// meaningful; the rest are zero.
    pub tracks: [TrackMeter; MAX_TRACKS],
    /// Tracks the graph was running when this block was measured.
    pub track_count: u8,
    /// Per-bus peaks, measured after each bus's strip, on the same terms as
    /// [`tracks`](Self::tracks). Only the first
    /// [`bus_count`](Self::bus_count) are meaningful.
    pub buses: [TrackMeter; MAX_BUSES],
    /// Buses the graph was running when this block was measured.
    pub bus_count: u8,
}

impl MeterFrame {
    /// All-zero frame at position 0, stopped, with no tracks.
    pub const SILENT: Self = Self {
        peak_l: 0.0,
        peak_r: 0.0,
        rms_l: 0.0,
        rms_r: 0.0,
        sample_pos: 0,
        playing: false,
        tracks: [TrackMeter::SILENT; MAX_TRACKS],
        track_count: 0,
        buses: [TrackMeter::SILENT; MAX_BUSES],
        bus_count: 0,
    };

    /// The tracks that actually exist, as a slice.
    pub fn track_meters(&self) -> &[TrackMeter] {
        &self.tracks[..(self.track_count as usize).min(MAX_TRACKS)]
    }

    /// The buses that actually exist, as a slice.
    pub fn bus_meters(&self) -> &[TrackMeter] {
        &self.buses[..(self.bus_count as usize).min(MAX_BUSES)]
    }

    /// Copy the graph's per-bus peaks into this frame. RT-safe.
    pub fn set_bus_peaks(&mut self, peaks: &BusPeaks) {
        self.bus_count = peaks.count().min(MAX_BUSES) as u8;
        for (slot, peak) in self.buses.iter_mut().zip(peaks.as_slice()) {
            slot.peak_l = peak.peak_l;
            slot.peak_r = peak.peak_r;
        }
    }

    /// Copy the graph's per-track peaks into this frame.
    ///
    /// RT-safe: a fixed-size copy, called once per block on the audio thread.
    pub fn set_track_peaks(&mut self, peaks: &TrackPeaks) {
        self.track_count = peaks.count().min(MAX_TRACKS) as u8;
        for (slot, peak) in self.tracks.iter_mut().zip(peaks.as_slice()) {
            slot.peak_l = peak.peak_l;
            slot.peak_r = peak.peak_r;
        }
    }
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
/// # Keeping to the grid
/// The deadline for the next emission advances by exactly `min_interval` from
/// the *previous deadline*, not from the moment the poll happened. That
/// difference is the whole of the spike-4 cadence bug: a caller polling on its
/// own timer never lands exactly on the deadline, and resetting to `now` folded
/// that overshoot into every subsequent interval, so a 30 Hz throttle polled
/// every 15 ms emitted at 22 Hz. Advancing the deadline instead means the lag is
/// bounded by one poll interval and never accumulates.
///
/// A caller that stalls for longer than [`RESYNC_AFTER`] intervals resyncs to
/// `now` rather than firing a burst to catch up: nobody wants six meter frames
/// at once after the pump was descheduled, and only the newest carries any
/// information anyway.
///
/// Lives on a non-RT thread (the IPC pump).
pub struct MeterThrottle {
    rx: Consumer<MeterFrame>,
    latest: Option<MeterFrame>,
    min_interval: Duration,
    /// Deadline the *next* emission is measured from. See the type docs: this is
    /// the previous ideal grid point, not the previous wall-clock emission.
    last_emit: Option<Instant>,
}

/// How far behind the grid the throttle may fall before it gives up on catching
/// up and resynchronises to the current time.
///
/// Two intervals: one poll's worth of slack is normal, anything more means the
/// caller was descheduled and a catch-up burst would help nobody.
pub const RESYNC_AFTER: u32 = 2;

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

        let elapsed = self
            .last_emit
            .map(|last| now.saturating_duration_since(last));
        if elapsed.is_some_and(|elapsed| elapsed < self.min_interval) {
            return None;
        }

        let frame = self.latest.take()?;
        self.last_emit = Some(match (self.last_emit, elapsed) {
            // Normal case: step the deadline forward by one whole interval, so
            // the overshoot of this poll is not baked into the next one.
            (Some(last), Some(elapsed)) if elapsed < self.min_interval * RESYNC_AFTER => {
                last + self.min_interval
            }
            // First emission, an unthrottled instance, or a caller that stalled:
            // start a fresh grid from here.
            _ => now,
        });
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

    /// The spike-4 cadence bug, as a regression test.
    ///
    /// A 30 Hz throttle polled every 15 ms used to emit at 21.85 Hz, because
    /// each emission reset the deadline to the moment of the (late) poll. It
    /// must now land within one poll interval of the ideal 30 per second.
    #[test]
    fn a_caller_polling_off_the_grid_still_gets_the_configured_rate() {
        for poll_ms in [5_u64, 8, 15, 16] {
            let (mut tx, rx) = rtrb::RingBuffer::<MeterFrame>::new(4);
            let mut throttle = MeterThrottle::with_rate(rx, 30.0);
            let t0 = Instant::now();

            let mut emitted = 0;
            let mut elapsed = 0_u64;
            while elapsed < 1_000 {
                let _ = tx.push(frame(0.5));
                if throttle.poll(t0 + Duration::from_millis(elapsed)).is_some() {
                    emitted += 1;
                }
                elapsed += poll_ms;
            }
            assert!(
                (29..=31).contains(&emitted),
                "polling every {poll_ms} ms should still yield ~30 frames/s, got {emitted}"
            );
        }
    }

    #[test]
    fn a_stalled_caller_resyncs_instead_of_firing_a_catch_up_burst() {
        let (mut tx, rx) = rtrb::RingBuffer::<MeterFrame>::new(8);
        let mut throttle = MeterThrottle::with_rate(rx, 30.0);
        let t0 = Instant::now();

        tx.push(frame(0.1)).unwrap();
        assert!(throttle.poll(t0).is_some());

        // Descheduled for a second, then polled tightly. A deadline that kept
        // stepping by 33 ms would owe ~30 frames and fire them back to back.
        let stall = Duration::from_millis(1_000);
        tx.push(frame(0.2)).unwrap();
        assert!(throttle.poll(t0 + stall).is_some());

        tx.push(frame(0.3)).unwrap();
        assert!(
            throttle
                .poll(t0 + stall + Duration::from_millis(1))
                .is_none(),
            "the grid restarts at the resync point, so nothing is owed"
        );
        tx.push(frame(0.4)).unwrap();
        assert!(throttle
            .poll(t0 + stall + Duration::from_millis(34))
            .is_some());
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
            ..MeterFrame::SILENT
        })
        .unwrap();
        assert_eq!(json["peak_l"], 0.5);
        assert_eq!(json["sample_pos"], 1024);
        assert_eq!(json["playing"], true);
        assert_eq!(json["track_count"], 0);
        assert_eq!(json["tracks"].as_array().unwrap().len(), MAX_TRACKS);
    }

    #[test]
    fn a_silent_frame_carries_no_tracks_at_all() {
        let frame = MeterFrame::SILENT;
        assert_eq!(frame.track_count, 0);
        assert!(frame.track_meters().is_empty());
    }

    #[test]
    fn track_peaks_from_the_graph_reach_the_frame_and_stop_at_the_track_count() {
        let mut peaks = mydaw_rt_core::TrackPeaks::empty(2);
        peaks.set(
            0,
            mydaw_rt_core::TrackPeak {
                peak_l: 0.5,
                peak_r: 0.25,
            },
        );
        let mut frame = MeterFrame::SILENT;
        frame.set_track_peaks(&peaks);

        assert_eq!(frame.track_count, 2);
        assert_eq!(frame.track_meters().len(), 2);
        assert_eq!(frame.tracks[0].peak_l, 0.5);
        assert_eq!(frame.tracks[0].peak_r, 0.25);
        assert_eq!(frame.tracks[1], TrackMeter::SILENT);
        // Everything past the count is left alone rather than reported.
        assert_eq!(frame.tracks[MAX_TRACKS - 1], TrackMeter::SILENT);
    }
}
