//! Live input monitoring: the microphone, heard through the mixer while armed.
//!
//! ```text
//!   capture callback (input device clock)        output graph (output device clock)
//!   InputRtSide::push_block --rtrb--> MonitorReader::render_add  (inside a node)
//! ```
//!
//! The two callbacks run on two devices whose clocks are never exactly equal,
//! and often not even nominally (a 44.1 kHz headset mic into a 48 kHz session).
//! So the reader is an *adaptive* resampler: linear interpolation at the
//! nominal ratio, nudged by how full the ring runs, the textbook way an audio
//! bridge between two clocks is built. Linear is enough for a cue signal --
//! what matters for a singer is latency, not a ruler-flat top octave.
//!
//! # Latency
//! The reader aims to keep only a small safety margin in the ring *at its
//! emptiest moment* (the minimum over a window of blocks), which is the lowest
//! latency the two callback schedules allow. An underrun means the margin was
//! too small: the reader goes silent, grows the margin and re-primes, so a
//! jittery machine settles at a safe latency on its own.
//!
//! # RT safety
//! Everything here is arithmetic over pre-allocated memory. The slot is a
//! `try_lock` -- never a blocking lock -- so a graph swap or a re-arm that is
//! holding it costs one block of silence, not a stalled callback.

use std::sync::Arc;

use parking_lot::Mutex;
use rtrb::{Consumer, Producer, RingBuffer};

use mydaw_rt_core::CHANNELS;

/// Monitor ring depth, in samples: about 170 ms of stereo at 48 kHz. Far
/// more than any latency worth monitoring at; the excess is only headroom for
/// the moment before a graph starts reading.
pub const MONITOR_QUEUE_CAPACITY: usize = 1 << 14;

/// Smallest and largest safety margin, in input frames.
const MIN_MARGIN: f64 = 48.0;
const MAX_MARGIN: f64 = 4_096.0;
/// Blocks per control window.
const WINDOW_BLOCKS: u32 = 24;
/// Largest rate correction, as a fraction of the nominal ratio (0.5 %): far
/// below anything audible as pitch, and far above any real clock mismatch.
const MAX_CORRECTION: f64 = 0.005;

/// Build both ends of a monitor ring. **Non-RT.**
pub fn monitor_channel() -> (Producer<f32>, Consumer<f32>) {
    RingBuffer::<f32>::new(MONITOR_QUEUE_CAPACITY)
}

/// The output side of one open input device.
pub struct MonitorReader {
    consumer: Consumer<f32>,
    /// Input device rate.
    device_rate: f64,
    /// Fractional read position between `a` and `b`.
    frac: f64,
    a: (f32, f32),
    b: (f32, f32),
    primed: bool,
    margin: f64,
    correction: f64,
    window_min: f64,
    window_blocks: u32,
    /// Output frames rendered in this window, to turn excess into a rate.
    window_frames: u64,
}

impl MonitorReader {
    /// A reader over `consumer`, fed at `device_rate`. **Non-RT.**
    pub fn new(consumer: Consumer<f32>, device_rate: f64) -> Self {
        Self {
            consumer,
            device_rate: if device_rate > 0.0 {
                device_rate
            } else {
                48_000.0
            },
            frac: 0.0,
            a: (0.0, 0.0),
            b: (0.0, 0.0),
            primed: false,
            margin: 256.0,
            correction: 0.0,
            window_min: f64::INFINITY,
            window_blocks: 0,
            window_frames: 0,
        }
    }

    fn available_frames(&self) -> f64 {
        (self.consumer.slots() / CHANNELS) as f64
    }

    fn pop(&mut self) -> Option<(f32, f32)> {
        if self.consumer.slots() < CHANNELS {
            return None;
        }
        let left = self.consumer.pop().ok()?;
        let right = self.consumer.pop().ok()?;
        let clean = |sample: f32| if sample.is_finite() { sample } else { 0.0 };
        Some((clean(left), clean(right)))
    }

    /// Mix `frames` of the live input into `left` / `right` at `engine_rate`.
    /// `capture_block` is the input device's block size, for priming.
    pub fn render_add(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        engine_rate: f64,
        capture_block: u32,
    ) {
        let frames = left.len().min(right.len());
        if frames == 0 || engine_rate <= 0.0 {
            return;
        }
        let available = self.available_frames();
        if !self.primed {
            // Wait until one capture block plus the margin is waiting, then
            // throw away anything older than that: the oldest audio is the
            // latency we do not want.
            let target = self.margin + f64::from(capture_block.max(1));
            if available < target {
                return;
            }
            let excess = (available - target) as usize;
            for _ in 0..excess {
                let _ = self.pop();
            }
            self.a = self.pop().unwrap_or((0.0, 0.0));
            self.b = self.pop().unwrap_or(self.a);
            self.frac = 0.0;
            self.primed = true;
            self.window_min = f64::INFINITY;
            self.window_blocks = 0;
            self.window_frames = 0;
        }

        let step = self.device_rate / engine_rate * (1.0 + self.correction);
        for frame in 0..frames {
            let t = self.frac as f32;
            left[frame] += self.a.0 + (self.b.0 - self.a.0) * t;
            right[frame] += self.a.1 + (self.b.1 - self.a.1) * t;
            self.frac += step;
            while self.frac >= 1.0 {
                self.frac -= 1.0;
                self.a = self.b;
                match self.pop() {
                    Some(next) => self.b = next,
                    None => {
                        // Underrun: the margin was too thin for this machine.
                        self.primed = false;
                        self.margin = (self.margin * 1.5 + 32.0).min(MAX_MARGIN);
                        self.correction = 0.0;
                        return;
                    }
                }
            }
        }

        // Track the emptiest the ring got, and steer toward the margin.
        self.window_min = self.window_min.min(self.available_frames());
        self.window_blocks += 1;
        self.window_frames += frames as u64;
        if self.window_blocks >= WINDOW_BLOCKS {
            let excess = self.window_min - self.margin;
            let input_frames = self.window_frames as f64 * self.device_rate / engine_rate;
            if excess > self.margin * 4.0 + 512.0 {
                // Far too much buffered (a stall upstream): drop it at once.
                for _ in 0..(excess - self.margin) as usize {
                    if self.pop().is_none() {
                        break;
                    }
                }
                self.correction = 0.0;
            } else if input_frames > 0.0 {
                // Drain the excess over roughly the next window.
                self.correction = (excess / input_frames).clamp(-MAX_CORRECTION, MAX_CORRECTION);
            }
            if excess > self.margin && self.margin > MIN_MARGIN {
                // Stable for a whole window with room to spare: try a little
                // less latency.
                self.margin = (self.margin * 0.9).max(MIN_MARGIN);
            }
            self.window_min = f64::INFINITY;
            self.window_blocks = 0;
            self.window_frames = 0;
        }
    }
}

/// Where the live input is handed to the output graph.
///
/// One per app. [`RecordService`](crate::RecordService) installs a reader when
/// a device opens and removes it when it closes; whichever graph node is the
/// monitor point renders from it.
#[derive(Default)]
pub struct MonitorSlot {
    reader: Mutex<Option<MonitorReader>>,
    /// The capture block size, mirrored from the open device.
    shared: Mutex<Option<Arc<crate::input::InputShared>>>,
}

impl MonitorSlot {
    /// An empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the reader. **Non-RT.**
    pub fn install(
        &self,
        reader: Option<MonitorReader>,
        shared: Option<Arc<crate::input::InputShared>>,
    ) {
        *self.reader.lock() = reader;
        *self.shared.lock() = shared;
    }

    /// Mix the live input into a block. **RT**: `try_lock` only.
    pub fn render_add(&self, left: &mut [f32], right: &mut [f32], engine_rate: f64) {
        let Some(shared) = self.shared.try_lock() else {
            return;
        };
        let Some(shared) = shared.as_ref() else {
            return;
        };
        if !shared.is_monitoring() {
            return;
        }
        let block = shared.block_frames();
        let Some(mut reader) = self.reader.try_lock() else {
            return;
        };
        if let Some(reader) = reader.as_mut() {
            reader.render_add(left, right, engine_rate, block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(producer: &mut Producer<f32>, frames: usize, value: f32) {
        for _ in 0..frames {
            producer.push(value).unwrap();
            producer.push(-value).unwrap();
        }
    }

    #[test]
    fn nothing_is_heard_until_the_ring_is_primed() {
        let (mut producer, consumer) = monitor_channel();
        let mut reader = MonitorReader::new(consumer, 48_000.0);
        feed(&mut producer, 10, 0.5);
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        reader.render_add(&mut l, &mut r, 48_000.0, 480);
        assert!(l.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_primed_reader_adds_the_input_at_unity() {
        let (mut producer, consumer) = monitor_channel();
        let mut reader = MonitorReader::new(consumer, 48_000.0);
        feed(&mut producer, 2_000, 0.25);
        let (mut l, mut r) = (vec![0.1; 128], vec![0.0; 128]);
        reader.render_add(&mut l, &mut r, 48_000.0, 480);
        assert!(l.iter().all(|s| (*s - 0.35).abs() < 1e-6), "{:?}", &l[..4]);
        assert!(r.iter().all(|s| (*s + 0.25).abs() < 1e-6));
    }

    #[test]
    fn priming_discards_old_audio_down_to_the_target_latency() {
        let (mut producer, consumer) = monitor_channel();
        let mut reader = MonitorReader::new(consumer, 48_000.0);
        feed(&mut producer, 6_000, 0.25);
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        reader.render_add(&mut l, &mut r, 48_000.0, 480);
        let left = reader.available_frames();
        assert!(left < 256.0 + 480.0, "{left} frames still queued");
    }

    #[test]
    fn an_underrun_goes_silent_and_grows_the_margin() {
        let (mut producer, consumer) = monitor_channel();
        let mut reader = MonitorReader::new(consumer, 48_000.0);
        feed(&mut producer, 800, 0.25);
        let margin = reader.margin;
        let (mut l, mut r) = (vec![0.0; 2_048], vec![0.0; 2_048]);
        reader.render_add(&mut l, &mut r, 48_000.0, 480);
        assert!(!reader.primed);
        assert!(reader.margin > margin);
    }

    #[test]
    fn a_slower_device_is_stretched_to_the_engine_rate() {
        let (mut producer, consumer) = monitor_channel();
        let mut reader = MonitorReader::new(consumer, 24_000.0);
        feed(&mut producer, 4_000, 0.5);
        let before = reader.available_frames();
        let (mut l, mut r) = (vec![0.0; 1_000], vec![0.0; 1_000]);
        reader.render_add(&mut l, &mut r, 48_000.0, 480);
        let used = before - reader.available_frames();
        // Priming dropped the excess first; after that 1000 output frames read
        // about 500 input frames.
        assert!(l.iter().all(|s| (*s - 0.5).abs() < 1e-6));
        assert!(used > 0.0);
    }

    #[test]
    fn a_slot_without_a_device_is_silent_and_does_not_block() {
        let slot = MonitorSlot::new();
        let (mut l, mut r) = (vec![0.0; 16], vec![0.0; 16]);
        slot.render_add(&mut l, &mut r, 48_000.0);
        assert!(l.iter().all(|s| *s == 0.0));
    }
}
