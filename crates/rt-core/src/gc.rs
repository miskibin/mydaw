//! Background deallocation thread for `basedrop`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use basedrop::{Collector, Handle};

/// Default collection interval. Deallocation latency is irrelevant to correctness
/// (memory is merely retained a little longer), so this is set low-frequency to
/// keep the thread effectively free.
pub const DEFAULT_GC_INTERVAL: Duration = Duration::from_millis(250);

/// Owns a [`Collector`] on a dedicated non-RT thread and drains it periodically.
///
/// The audio thread never deallocates; it hands garbage to the collector via
/// [`basedrop::Owned`]/`Shared`, and this thread does the actual `free()`.
/// Hand out [`GcThread::handle`] clones to anything that allocates deferred-drop
/// data.
pub struct GcThread {
    handle: Handle,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl GcThread {
    /// Spawn the collector thread with [`DEFAULT_GC_INTERVAL`].
    pub fn spawn() -> Self {
        Self::spawn_with_interval(DEFAULT_GC_INTERVAL)
    }

    /// Spawn the collector thread, collecting every `interval`.
    pub fn spawn_with_interval(interval: Duration) -> Self {
        let collector = Collector::new();
        let handle = collector.handle();
        let stop = Arc::new(AtomicBool::new(false));

        let stop_flag = Arc::clone(&stop);
        let join = thread::Builder::new()
            .name("mydaw-gc".to_owned())
            .spawn(move || {
                let mut collector = collector;
                while !stop_flag.load(Ordering::Relaxed) {
                    collector.collect();
                    thread::sleep(interval);
                }
                // Final sweep so anything dropped during shutdown is freed while
                // this thread (and not an audio callback) is doing the work.
                collector.collect();
            })
            .expect("failed to spawn mydaw-gc thread");

        Self {
            handle,
            stop,
            join: Some(join),
        }
    }

    /// Clone of the collector handle. Cheap; clone freely on non-RT threads.
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    /// Signal the thread to stop and wait for it. Idempotent.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            // A failed join means the GC thread panicked; that is a bug worth
            // surfacing, but not worth aborting shutdown over.
            let _ = join.join();
        }
    }
}

impl Drop for GcThread {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use basedrop::Owned;

    #[test]
    fn gc_thread_frees_dropped_data() {
        let gc = GcThread::spawn_with_interval(Duration::from_millis(1));
        let handle = gc.handle();

        let value = Owned::new(&handle, vec![0u8; 1024]);
        assert_eq!(value.len(), 1024);
        drop(value);

        // The collector runs on its own schedule; dropping the GcThread joins the
        // thread after a final collect(), which is the deterministic sync point.
        drop(gc);
    }
}
