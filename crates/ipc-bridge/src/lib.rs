//! `mydaw-ipc-bridge` -- the logic half of the kernel <-> webview boundary.
//!
//! Deliberately **Tauri-free**: this crate holds the data shapes and the rate
//! limiting, and knows nothing about Tauri commands, channels or windows. The
//! `app` crate owns the Tauri dependency and does nothing but wire these types to
//! `#[tauri::command]`s and `Channel`s. That keeps everything here unit-testable
//! without spinning up a webview.
//!
//! Two directions:
//! - **Pull**: [`UiSnapshot`] -- kernel/plugin/panel state, requested by the UI.
//! - **Push**: [`MeterFrame`] via [`MeterThrottle`] -- high-rate audio-thread data,
//!   decimated to a UI-appropriate frame rate before it crosses the IPC boundary.
//!
//! # RT safety
//! Nothing here runs on the audio thread. The audio thread only pushes
//! [`MeterFrame`]s (plain `Copy` data) into an `rtrb` queue; [`MeterThrottle`]
//! drains that queue from the non-RT IPC pump.

mod dto;
mod meter;

pub use dto::{PanelManifestDto, PluginStateDto, PluginStatusDto, UiSnapshot};
pub use meter::{MeterFrame, MeterThrottle, TrackMeter, DEFAULT_METER_HZ};

/// Tracks a [`MeterFrame`] can carry. The engine's own limit, re-exported so the
/// UI side has one number to mirror.
pub use mydaw_rt_core::MAX_TRACKS;

/// Producer half of the meter queue; lives on the audio thread.
pub type MeterProducer = rtrb::Producer<MeterFrame>;
/// Consumer half of the meter queue; owned by [`MeterThrottle`].
pub type MeterConsumer = rtrb::Consumer<MeterFrame>;

/// Create the audio-thread -> UI meter queue.
///
/// Allocates here, on the calling (non-RT) thread. `capacity` only needs to cover
/// the number of blocks between two IPC pumps; the throttle drains the queue
/// completely on every poll, so overruns just mean a dropped intermediate frame.
pub fn meter_channel(capacity: usize) -> (MeterProducer, MeterThrottle) {
    let (tx, rx) = rtrb::RingBuffer::new(capacity);
    (tx, MeterThrottle::new(rx))
}

/// Same as [`meter_channel`] but with an explicit emission rate in Hz.
pub fn meter_channel_with_rate(capacity: usize, rate_hz: f64) -> (MeterProducer, MeterThrottle) {
    let (tx, rx) = rtrb::RingBuffer::new(capacity);
    (tx, MeterThrottle::with_rate(rx, rate_hz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn meter_channel_roundtrip() {
        let (mut tx, mut throttle) = meter_channel(8);
        tx.push(MeterFrame {
            peak_l: 1.0,
            sample_pos: 512,
            playing: true,
            ..MeterFrame::SILENT
        })
        .unwrap();
        let frame = throttle
            .poll(Instant::now())
            .expect("frame should be emitted");
        assert_eq!(frame.peak_l, 1.0);
        assert_eq!(frame.sample_pos, 512);
        assert!(frame.playing);
    }

    #[test]
    fn meter_channel_with_rate_sets_interval() {
        let (_tx, throttle) = meter_channel_with_rate(4, 60.0);
        assert_eq!(
            throttle.min_interval(),
            std::time::Duration::from_secs_f64(1.0 / 60.0)
        );
    }
}
