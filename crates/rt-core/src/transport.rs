//! Transport clock shared between the audio thread and the rest of the app.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Immutable transport state for the duration of one block.
///
/// Taken once per block ([`TransportClock::snapshot`]) so that every node in the
/// graph sees the *same* position, instead of racing on the atomics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportSnapshot {
    /// Sample position of the first frame of this block.
    pub sample_pos: u64,
    pub playing: bool,
}

/// Sample-accurate play position, shared via `Arc`.
///
/// # RT safety
/// Plain atomics, no locks: the audio thread reads a snapshot at the top of the
/// block and calls [`advance`](Self::advance) at the bottom. Non-RT threads (UI,
/// IPC) may read at any time; they get a possibly-one-block-stale value, which
/// is exactly what the UI wants (it interpolates the playhead locally anyway).
pub struct TransportClock {
    sample_pos: AtomicU64,
    playing: AtomicBool,
    sample_rate: f64,
}

impl TransportClock {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            sample_pos: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            sample_rate,
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Read the current state. RT-safe.
    pub fn snapshot(&self) -> TransportSnapshot {
        TransportSnapshot {
            sample_pos: self.sample_pos.load(Ordering::Relaxed),
            playing: self.playing.load(Ordering::Relaxed),
        }
    }

    /// Advance the play head by `frames`, but only while playing.
    ///
    /// Called by the audio thread *after* a block has been processed, so that
    /// `snapshot().sample_pos` always names the first frame of the block that is
    /// currently being rendered.
    pub fn advance(&self, frames: usize) {
        if self.playing.load(Ordering::Relaxed) {
            self.sample_pos.fetch_add(frames as u64, Ordering::Relaxed);
        }
    }

    pub fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Jump to an absolute sample position.
    pub fn seek(&self, sample_pos: u64) {
        self.sample_pos.store(sample_pos, Ordering::Relaxed);
    }

    /// Current position in seconds, for display purposes.
    pub fn position_seconds(&self) -> f64 {
        self.sample_pos.load(Ordering::Relaxed) as f64 / self.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_advance_while_stopped() {
        let t = TransportClock::new(48_000.0);
        t.advance(512);
        assert_eq!(t.snapshot().sample_pos, 0);
    }

    #[test]
    fn advances_while_playing() {
        let t = TransportClock::new(48_000.0);
        t.set_playing(true);
        t.advance(512);
        t.advance(512);
        let s = t.snapshot();
        assert_eq!(s.sample_pos, 1024);
        assert!(s.playing);
    }

    #[test]
    fn seek_and_seconds() {
        let t = TransportClock::new(48_000.0);
        t.seek(48_000);
        assert_eq!(t.snapshot().sample_pos, 48_000);
        assert!((t.position_seconds() - 1.0).abs() < f64::EPSILON);
    }
}
