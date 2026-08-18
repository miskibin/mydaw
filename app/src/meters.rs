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

/// Fallback poll interval, used when there is no throttle to derive one from.
///
/// With a throttle, [`poll_interval_for`] divides its emission interval instead;
/// see that function for why a fixed 15 ms was the wrong answer.
pub const POLL_INTERVAL: Duration = Duration::from_millis(15);

/// How many times the pump looks at the queue per emission slot.
///
/// Four gives a worst-case quantisation lag of a quarter of a frame interval
/// (~8 ms at 30 Hz) for four wake-ups per slot on an idle thread, which is the
/// cheap end of a trade that only gets more expensive.
pub const POLL_DIVISOR: u32 = 4;

/// Shortest interval the pump will spin at, whatever the throttle asks for.
///
/// A 240 Hz throttle would otherwise ask for a 1 ms sleep loop; the pump is not
/// a real-time thread and should not pretend to be one.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Poll interval for a throttle emitting every `emit_interval`.
///
/// # Why this is not a constant
/// The throttle decides *whether* to emit; the pump decides how promptly an
/// emission is noticed. Spike 4 measured the live meter path at 21.85 Hz instead
/// of 30, and the transport was not to blame: a 15 ms poll against a 33.3 ms
/// budget means the poll at 30 ms is refused and the frame waits until 45 ms, so
/// the real emission grid was 45 ms -- 22.2 Hz. Two independent timers beating
/// against each other.
///
/// Dividing the throttle's own interval makes the poll grid a divisor of the
/// emission grid, so every deadline is served by the next poll rather than the
/// one after it. Together with the throttle's deadline stepping (see
/// [`MeterThrottle`]) the long-run rate is the configured one.
pub fn poll_interval_for(emit_interval: Duration) -> Duration {
    if emit_interval.is_zero() {
        // An unthrottled throttle emits whenever asked; poll at the default.
        return POLL_INTERVAL;
    }
    (emit_interval / POLL_DIVISOR).max(MIN_POLL_INTERVAL)
}

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
    /// Whether to stamp each frame with the bench clock on its way out.
    ///
    /// Off unless the IPC benchmark turns it on, and one relaxed atomic load per
    /// emitted frame (at most 30 a second, on a thread that is about to do IPC)
    /// when it is. This is the *only* change spike 4 makes to the live meter
    /// path, and it is what makes the real end-to-end meter latency measurable
    /// rather than inferred.
    stamp: AtomicBool,
}

impl MeterPump {
    /// Pump over the metering plugin's throttle, polling fast enough to serve
    /// its emission rate. See [`poll_interval_for`].
    pub fn new(throttle: Option<MeterThrottle>) -> Arc<Self> {
        let interval = throttle
            .as_ref()
            .map_or(POLL_INTERVAL, |t| poll_interval_for(t.min_interval()));
        Self::with_interval(throttle, interval)
    }

    /// How often this pump looks at the queue.
    pub fn poll_interval(&self) -> Duration {
        self.interval
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
            stamp: AtomicBool::new(false),
        })
    }

    /// Start or stop stamping emitted frames with the bench clock.
    ///
    /// Only `bench_meter_stamp` calls this; see [`MeterFrameDto::pump_ms`].
    pub fn set_stamping(&self, enabled: bool) {
        self.stamp.store(enabled, Ordering::Relaxed);
    }

    /// Whether frames currently carry a bench timestamp.
    pub fn is_stamping(&self) -> bool {
        self.stamp.load(Ordering::Relaxed)
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
        let mut dto = MeterFrameDto::from(frame);
        if self.stamp.load(Ordering::Relaxed) {
            // Stamped here, immediately before the send, so the number the
            // webview subtracts covers the IPC hop and nothing else.
            dto.pump_ms = Some(crate::ipc_bench::epoch_ms());
        }
        if sink(dto) {
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
    fn frames_are_stamped_only_while_the_bench_asks_for_it() {
        let (mut tx, pump) = rig();
        let seen: Arc<Mutex<Vec<Option<f64>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        pump.subscribe(Box::new(move |dto| {
            sink_seen.lock().push(dto.pump_ms);
            true
        }));

        // Explicit, spaced instants: back-to-back polls would be swallowed by
        // the throttle rather than by anything this test is about.
        let t0 = Instant::now();
        let tick = Duration::from_millis(10);

        assert!(!pump.is_stamping(), "the live path is unstamped by default");
        tx.push(frame(0.5)).unwrap();
        assert!(pump.pump_once(t0));

        pump.set_stamping(true);
        assert!(pump.is_stamping());
        tx.push(frame(0.5)).unwrap();
        assert!(pump.pump_once(t0 + tick));

        pump.set_stamping(false);
        tx.push(frame(0.5)).unwrap();
        assert!(pump.pump_once(t0 + tick * 2));

        let seen = seen.lock();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0], None);
        assert!(seen[1].is_some_and(|ms| ms > 0.0), "stamped while enabled");
        assert_eq!(seen[2], None, "stamping is switched off again");

        pump.shutdown();
    }

    #[test]
    fn a_pump_without_a_throttle_is_harmless() {
        let pump = MeterPump::new(None);
        assert!(!pump.has_throttle());
        assert_eq!(pump.poll_interval(), POLL_INTERVAL);
        pump.subscribe(Box::new(|_| true));
        assert!(!pump.pump_once(Instant::now()));
        pump.shutdown();
    }

    #[test]
    fn the_poll_interval_divides_the_throttles_emission_interval() {
        // 30 Hz -> 33.33 ms -> 8.33 ms, a quarter of the emission slot.
        let thirty_hz = Duration::from_secs_f64(1.0 / 30.0);
        let polled = poll_interval_for(thirty_hz);
        assert_eq!(polled, thirty_hz / POLL_DIVISOR);
        assert!(polled < thirty_hz, "the pump must outpace the throttle");

        // A very fast throttle is clamped: the pump is not an RT thread.
        assert_eq!(
            poll_interval_for(Duration::from_millis(4)),
            MIN_POLL_INTERVAL
        );
        // An unthrottled throttle has no interval to divide.
        assert_eq!(poll_interval_for(Duration::ZERO), POLL_INTERVAL);
    }

    /// The spike-4 finding, end to end through the pump: a 30 Hz throttle
    /// polled at the pump's own interval must deliver ~30 frames a second, not
    /// the 21.85 that a 15 ms poll against a 33.3 ms budget used to produce.
    #[test]
    fn a_thirty_hertz_throttle_delivers_thirty_frames_a_second_through_the_pump() {
        let (mut tx, throttle) = mydaw_ipc_bridge::meter_channel(16);
        let pump = MeterPump::new(Some(throttle));
        let interval = pump.poll_interval();
        assert!(interval < Duration::from_millis(15), "{interval:?}");

        let delivered = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&delivered);
        pump.subscribe(Box::new(move |_| {
            sink.fetch_add(1, Ordering::Relaxed);
            true
        }));

        // Simulated wall clock: the thread body's `sleep(interval)` loop, but
        // deterministic, so this test cannot flake on a busy machine.
        let t0 = Instant::now();
        let mut elapsed = Duration::ZERO;
        while elapsed < Duration::from_secs(1) {
            let _ = tx.push(frame(0.5));
            pump.pump_once(t0 + elapsed);
            elapsed += interval;
        }

        let count = delivered.load(Ordering::Relaxed);
        assert!(
            (29..=31).contains(&count),
            "expected ~30 frames in one second, got {count}"
        );
        pump.shutdown();
    }
}
