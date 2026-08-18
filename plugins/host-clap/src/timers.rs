//! Host-side `clap.timer-support`: the heartbeat a plugin editor animates on.
//!
//! CLAP makes timers the **host's** job. A plugin asks for "call me every 33 ms"
//! and the host is expected to call `on_timer` on the main thread at roughly that
//! rate, forever, without the plugin owning a thread or a Win32 timer of its own.
//! Most Windows plugin editors will not repaint at all without this.
//!
//! This module is only the registry: which timers exist, and which are due.
//! Dispatch lives in [`LoadedClapPlugin::tick_main_thread`], because calling into
//! the plugin needs the instance.
//!
//! [`LoadedClapPlugin::tick_main_thread`]: crate::hosting::LoadedClapPlugin::tick_main_thread

use std::time::{Duration, Instant};

use clack_extensions::timer::TimerId;
use parking_lot::Mutex;

/// Shortest period this host will honour, in milliseconds.
///
/// The main loop is woken at ~60 Hz by the plugin window's own timer, so a
/// promise of 1 ms would be one the host cannot keep. CLAP explicitly allows the
/// host to lengthen a period it considers too short, and asks that at least 30 Hz
/// be available -- 8 ms leaves room above that.
pub const MIN_PERIOD_MS: u32 = 8;

/// Longest period, in milliseconds. Guards against a plugin passing 0-as-u32-max
/// style nonsense; an hour is far beyond any editor's idle tick.
pub const MAX_PERIOD_MS: u32 = 60 * 60 * 1000;

/// One registered timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timer {
    id: TimerId,
    period: Duration,
    due: Instant,
}

/// The plugin's timer registrations.
///
/// Behind a `Mutex` because CLAP declares `register_timer` `[main-thread]` while
/// the host's own bookkeeping is reached from the same thread through two
/// different paths (the plugin's callback, and the host's tick). The lock is
/// never held across a call into the plugin -- see
/// [`collect_due`](Self::collect_due).
#[derive(Debug)]
pub struct ClapTimers {
    inner: Mutex<Registry>,
}

#[derive(Debug, Default)]
struct Registry {
    next_id: u32,
    timers: Vec<Timer>,
}

impl Default for ClapTimers {
    fn default() -> Self {
        Self::new()
    }
}

impl ClapTimers {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Registry::default()),
        }
    }

    /// Register a timer with a period clamped into
    /// [`MIN_PERIOD_MS`]..=[`MAX_PERIOD_MS`], returning its new id.
    pub fn register(&self, period_ms: u32) -> TimerId {
        self.register_at(period_ms, Instant::now())
    }

    /// [`register`](Self::register) with an explicit clock, for tests.
    pub fn register_at(&self, period_ms: u32, now: Instant) -> TimerId {
        let period =
            Duration::from_millis(u64::from(period_ms.clamp(MIN_PERIOD_MS, MAX_PERIOD_MS)));
        let mut inner = self.inner.lock();
        let id = TimerId(inner.next_id);
        inner.next_id = inner.next_id.wrapping_add(1);
        inner.timers.push(Timer {
            id,
            period,
            due: now + period,
        });
        id
    }

    /// Remove a timer. Returns whether it existed -- a plugin unregistering a
    /// timer twice is a plugin bug the host reports rather than hides.
    pub fn unregister(&self, id: TimerId) -> bool {
        let mut inner = self.inner.lock();
        let before = inner.timers.len();
        inner.timers.retain(|timer| timer.id != id);
        inner.timers.len() != before
    }

    /// Whether `id` is still registered.
    pub fn contains(&self, id: TimerId) -> bool {
        self.inner.lock().timers.iter().any(|timer| timer.id == id)
    }

    /// How many timers are registered.
    pub fn len(&self) -> usize {
        self.inner.lock().timers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The period a timer was registered with, if it exists. For tests and logs.
    pub fn period(&self, id: TimerId) -> Option<Duration> {
        self.inner
            .lock()
            .timers
            .iter()
            .find(|timer| timer.id == id)
            .map(|timer| timer.period)
    }

    /// Fill `out` with the timers due at `now`, and schedule their next tick.
    ///
    /// `out` is cleared first and is the caller's buffer so that a 60 Hz tick
    /// allocates nothing. The lock is released before the caller dispatches,
    /// which is what makes it legal for `on_timer` to register or unregister
    /// timers.
    ///
    /// A timer that fell far behind (a modal drag, a stalled loop) is *not*
    /// replayed: its next deadline is computed from `now`, so a two-second stall
    /// costs one tick, not sixty. Plugins expect a heartbeat, not a backlog.
    pub fn collect_due(&self, now: Instant, out: &mut Vec<TimerId>) {
        out.clear();
        let mut inner = self.inner.lock();
        for timer in &mut inner.timers {
            if now >= timer.due {
                out.push(timer.id);
                timer.due = now + timer.period;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_timer_becomes_due_after_its_period() {
        let timers = ClapTimers::new();
        let start = Instant::now();
        let id = timers.register_at(50, start);
        assert_eq!(timers.len(), 1);
        assert!(timers.contains(id));
        assert_eq!(timers.period(id), Some(Duration::from_millis(50)));

        let mut due = Vec::new();
        timers.collect_due(start, &mut due);
        assert!(due.is_empty(), "a fresh timer is not immediately due");

        timers.collect_due(start + Duration::from_millis(49), &mut due);
        assert!(due.is_empty());

        timers.collect_due(start + Duration::from_millis(50), &mut due);
        assert_eq!(due, vec![id]);

        // And it does not fire again until another period has passed.
        timers.collect_due(start + Duration::from_millis(60), &mut due);
        assert!(due.is_empty());
        timers.collect_due(start + Duration::from_millis(101), &mut due);
        assert_eq!(due, vec![id]);
    }

    #[test]
    fn a_stall_costs_one_tick_not_a_backlog() {
        let timers = ClapTimers::new();
        let start = Instant::now();
        let id = timers.register_at(10, start);

        let mut due = Vec::new();
        // Two seconds of nothing: 200 periods missed.
        timers.collect_due(start + Duration::from_secs(2), &mut due);
        assert_eq!(due, vec![id], "the backlog must not be replayed");

        // The next deadline is measured from the late tick, not the old one.
        timers.collect_due(start + Duration::from_secs(2), &mut due);
        assert!(due.is_empty());
        timers.collect_due(
            start + Duration::from_secs(2) + Duration::from_millis(10),
            &mut due,
        );
        assert_eq!(due, vec![id]);
    }

    #[test]
    fn periods_are_clamped_to_what_this_host_can_deliver() {
        let timers = ClapTimers::new();
        let fast = timers.register(0);
        let slow = timers.register(u32::MAX);
        assert_eq!(
            timers.period(fast),
            Some(Duration::from_millis(u64::from(MIN_PERIOD_MS)))
        );
        assert_eq!(
            timers.period(slow),
            Some(Duration::from_millis(u64::from(MAX_PERIOD_MS)))
        );
    }

    #[test]
    fn timers_have_distinct_ids_and_can_be_unregistered() {
        let timers = ClapTimers::new();
        let first = timers.register(20);
        let second = timers.register(20);
        assert_ne!(first, second);
        assert_eq!(timers.len(), 2);

        assert!(timers.unregister(first));
        assert!(!timers.contains(first));
        assert!(
            !timers.unregister(first),
            "unregistering twice must report the truth"
        );
        assert_eq!(timers.len(), 1);

        assert!(timers.unregister(second));
        assert!(timers.is_empty());
    }

    #[test]
    fn several_timers_fire_independently() {
        let timers = ClapTimers::new();
        let start = Instant::now();
        let fast = timers.register_at(10, start);
        let slow = timers.register_at(100, start);

        let mut due = Vec::new();
        timers.collect_due(start + Duration::from_millis(10), &mut due);
        assert_eq!(due, vec![fast]);

        timers.collect_due(start + Duration::from_millis(100), &mut due);
        assert_eq!(due.len(), 2, "both are due at 100 ms, got {due:?}");
        assert!(due.contains(&fast) && due.contains(&slow));
    }

    #[test]
    fn collecting_reuses_the_callers_buffer() {
        let timers = ClapTimers::new();
        let start = Instant::now();
        timers.register_at(10, start);

        let mut due = vec![TimerId(999)];
        timers.collect_due(start, &mut due);
        assert!(due.is_empty(), "stale entries must be cleared");
    }
}
