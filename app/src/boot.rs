//! Startup: what the window waits for, and a trace of where the time goes.
//!
//! The window must appear before any slow work. Reopening the last project decodes every
//! audio file in it, which takes seconds on a big session, so it runs on its own thread
//! while the webview loads. The UI asks for the project state exactly once, at boot
//! (`get_state`); [`Boot::wait`] holds that one reply until the project is in, so the UI
//! never starts on an empty project that the load then replaces.
//!
//! `WAVE_STARTUP_TRACE=1` prints the timeline of a launch to stderr.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

pub struct Boot {
    t0: Instant,
    trace: bool,
    loaded: Mutex<bool>,
    cv: Condvar,
}

/// Dropping it, even by a panic in the loader, releases everyone waiting for the project.
pub struct Loading(Arc<Boot>);

impl Boot {
    pub fn new() -> Arc<Self> {
        let trace = std::env::var_os("WAVE_STARTUP_TRACE").is_some_and(|v| v != "0");
        Arc::new(Boot { t0: Instant::now(), trace, loaded: Mutex::new(false), cv: Condvar::new() })
    }

    /// Prints `what` with the time since the process entered `run()` (when tracing).
    pub fn mark(&self, what: &str) {
        if self.trace {
            eprintln!("[startup {:>6.1} ms] {what}", self.t0.elapsed().as_secs_f64() * 1000.0);
        }
    }

    /// The guard of the load in progress.
    pub fn loading(self: &Arc<Self>) -> Loading {
        Loading(self.clone())
    }

    /// Blocks until the startup load is over (done or failed), at most `limit`.
    /// Returns whether it finished: a stuck disk must not keep the UI away for ever.
    pub fn wait(&self, limit: Duration) -> bool {
        let mut done = self.loaded.lock();
        if !*done {
            self.cv.wait_for(&mut done, limit);
        }
        *done
    }
}

impl Drop for Loading {
    fn drop(&mut self) {
        *self.0.loaded.lock() = true;
        self.0.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_returns_once_the_load_guard_drops() {
        let b = Boot::new();
        let guard = b.loading();
        let b2 = b.clone();
        let waiter = std::thread::spawn(move || b2.wait(Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(30));
        drop(guard);
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn a_panicking_load_still_releases_the_waiter() {
        let b = Boot::new();
        let guard = b.loading();
        let t = std::thread::spawn(move || {
            let _g = guard;
            panic!("decoder blew up");
        });
        assert!(t.join().is_err());
        assert!(b.wait(Duration::from_secs(10)));
    }

    #[test]
    fn a_stuck_load_times_out_instead_of_hanging_the_ui() {
        let b = Boot::new();
        let _guard = b.loading();
        assert!(!b.wait(Duration::from_millis(40)));
    }

    #[test]
    fn nothing_to_wait_for_after_the_load() {
        let b = Boot::new();
        drop(b.loading());
        assert!(b.wait(Duration::ZERO));
    }
}
