//! The meter pump: the non-RT thread that moves frames from the audio thread's
//! queue to the webview.
//!
//! ```text
//!   audio thread          rtrb queue          pump thread            webview
//!   MeterFrame push  -->  (metering)  -->  MeterThrottle.poll  -->  Channel.send
//!                                          (<= 30 Hz)
//! ```
//!
//! The pump owns no Tauri types: it pushes into a [`FrameSink`] closure, which
//! the `subscribe_meters` command fills with a `Channel::send`. That keeps the
//! rate limiting, the single-subscription rule and the teardown path testable
//! without a running webview.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mydaw_ipc_bridge::MeterThrottle;
use parking_lot::Mutex;

use crate::dto::MeterFrameDto;

/// Where a polled frame goes. Returns `false` when the receiver is gone, which
/// retires the subscription.
pub type FrameSink = Box<dyn Fn(MeterFrameDto) -> bool + Send + Sync + 'static>;

/// How often the pump looks at the queue.
///
/// Deliberately faster than the throttle's own 30 Hz budget: the throttle decides
/// *whether* to emit, the pump only decides how promptly an emission is noticed.
/// 15 ms keeps the added jitter at half a frame interval.
pub const POLL_INTERVAL: Duration = Duration::from_millis(15);

/// Drains the throttled meter queue into the current subscriber.
pub struct MeterPump {
    /// `None` when the metering plugin's throttle was already claimed elsewhere;
    /// the pump then runs but never has anything to emit.
    throttle: Mutex<Option<MeterThrottle>>,
    /// Exactly one subscriber at a time. A second `subscribe_meters` (a webview
    /// reload, say) replaces the first rather than fanning out: two webviews are
    /// not a Phase 0 concern, and a stale channel that nobody retires would leak.
    sink: Mutex<Option<FrameSink>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    running: AtomicBool,
    shutdown: AtomicBool,
    interval: Duration,
}

impl MeterPump {
    /// Pump over the metering plugin's throttle.
    pub fn new(throttle: Option<MeterThrottle>) -> Arc<Self> {
        Self::with_interval(throttle, POLL_INTERVAL)
    }

    /// Pump with an explicit poll interval (tests).
    pub fn with_interval(throttle: Option<MeterThrottle>, interval: Duration) -> Arc<Self> {
        Arc::new(Self {
            throttle: Mutex::new(throttle),
            sink: Mutex::new(None),
            thread: Mutex::new(None),
            running: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            interval,
        })
    }

    /// Whether a throttle was handed over at construction.
    pub fn has_throttle(&self) -> bool {
        self.throttle.lock().is_some()
    }

    /// Whether a subscriber is currently attached.
    pub fn has_subscriber(&self) -> bool {
        self.sink.lock().is_some()
    }

    /// Whether the pump thread is alive.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Install `sink` as *the* subscriber, starting the pump thread on first use.
    ///
    /// Replacing an existing subscriber is intentional and silent-but-logged: the
    /// webview reloading during development is the common case.
    pub fn subscribe(self: &Arc<Self>, sink: FrameSink) {
        if self.shutdown.load(Ordering::Acquire) {
            log::warn!("meter subscription requested after shutdown; ignoring");
            return;
        }
        if self.sink.lock().replace(sink).is_some() {
            log::info!("meter subscription replaced by a newer one");
        }
        self.ensure_thread();
    }

    /// Drop the current subscriber without stopping the pump.
    pub fn unsubscribe(&self) {
        self.sink.lock().take();
    }

    /// One pump tick: drain the queue, emit at most one frame.
    ///
    /// Split out from the thread body so the rate limiting and the
    /// dead-subscriber path can be tested directly. Returns whether a frame was
    /// delivered.
    pub fn pump_once(&self, now: Instant) -> bool {
        let frame = {
            let mut guard = self.throttle.lock();
            match guard.as_mut() {
                Some(throttle) => throttle.poll(now),
                None => None,
            }
        };
        let Some(frame) = frame else {
            return false;
        };

        let mut guard = self.sink.lock();
        let Some(sink) = guard.as_ref() else {
            return false;
        };
        if sink(MeterFrameDto::from(frame)) {
            true
        } else {
            // The webview went away (reload, close). Retire the subscription so
            // the pump stops paying for sends nobody receives.
            log::debug!("meter subscriber is gone; retiring the subscription");
            *guard = None;
            false
        }
    }

    /// Stop the pump thread and drop the subscriber. Idempotent.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.unsubscribe();
        if let Some(handle) = self.thread.lock().take() {
            let _ = handle.join();
        }
        self.running.store(false, Ordering::Release);
    }

    fn ensure_thread(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let pump = Arc::clone(self);
        let handle = thread::Builder::new()
            .name("mydaw-meter-pump".to_owned())
            .spawn(move || {
                while !pump.shutdown.load(Ordering::Acquire) {
                    pump.pump_once(Instant::now());
                    thread::sleep(pump.interval);
                }
            });
        match handle {
            Ok(handle) => *self.thread.lock() = Some(handle),
            Err(err) => {
                log::error!("could not spawn the meter pump thread: {err}");
                self.running.store(false, Ordering::Release);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_ipc_bridge::{meter_channel_with_rate, MeterFrame, MeterProducer};
    use std::sync::atomic::AtomicUsize;

    fn rig() -> (MeterProducer, Arc<MeterPump>) {
        // 10 kHz throttle: the rate limiter is `ipc-bridge`'s tested concern, and
        // these tests are about the pump around it.
        let (tx, throttle) = meter_channel_with_rate(16, 10_000.0);
        (tx, MeterPump::new(Some(throttle)))
    }

    fn frame(peak: f32) -> MeterFrame {
        MeterFrame {
            peak_l: peak,
            peak_r: peak,
            ..MeterFrame::SILENT
        }
    }

    #[test]
    fn without_a_subscriber_frames_are_dropped_not_queued() {
        let (mut tx, pump) = rig();
        tx.push(frame(0.5)).unwrap();
        assert!(!pump.pump_once(Instant::now()));
        assert!(!pump.has_subscriber());
    }

    #[test]
    fn a_subscriber_receives_the_newest_frame() {
        let (mut tx, pump) = rig();
        let seen = Arc::new(Mutex::new(Vec::<f32>::new()));
        let sink_seen = Arc::clone(&seen);
        pump.subscribe(Box::new(move |dto| {
            sink_seen.lock().push(dto.peak_l);
            true
        }));

        tx.push(frame(0.1)).unwrap();
        tx.push(frame(0.9)).unwrap();
        assert!(pump.pump_once(Instant::now()));
        assert_eq!(*seen.lock(), vec![0.9]);

        pump.shutdown();
    }

    #[test]
    fn subscribing_twice_replaces_the_first_subscriber() {
        let (mut tx, pump) = rig();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));

        let a = Arc::clone(&first);
        pump.subscribe(Box::new(move |_| {
            a.fetch_add(1, Ordering::Relaxed);
            true
        }));
        let b = Arc::clone(&second);
        pump.subscribe(Box::new(move |_| {
            b.fetch_add(1, Ordering::Relaxed);
            true
        }));

        tx.push(frame(0.5)).unwrap();
        assert!(pump.pump_once(Instant::now()));
        assert_eq!(first.load(Ordering::Relaxed), 0);
        assert_eq!(second.load(Ordering::Relaxed), 1);

        pump.shutdown();
    }

    #[test]
    fn a_dead_subscriber_is_retired() {
        let (mut tx, pump) = rig();
        pump.subscribe(Box::new(|_| false));

        tx.push(frame(0.5)).unwrap();
        assert!(!pump.pump_once(Instant::now()));
        assert!(!pump.has_subscriber(), "a failing sink must be dropped");

        pump.shutdown();
    }

    #[test]
    fn the_thread_starts_on_subscribe_and_stops_on_shutdown() {
        let (_tx, pump) = rig();
        assert!(!pump.is_running());
        pump.subscribe(Box::new(|_| true));
        assert!(pump.is_running());

        pump.shutdown();
        assert!(!pump.is_running());
        assert!(!pump.has_subscriber());

        // Late subscriptions after shutdown do not resurrect the thread.
        pump.subscribe(Box::new(|_| true));
        assert!(!pump.is_running());
    }

    #[test]
    fn a_pump_without_a_throttle_is_harmless() {
        let pump = MeterPump::new(None);
        assert!(!pump.has_throttle());
        pump.subscribe(Box::new(|_| true));
        assert!(!pump.pump_once(Instant::now()));
        pump.shutdown();
    }
}
