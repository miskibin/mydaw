//! Running plugin work on the UI ("main") thread.
//!
//! VST3 puts everything except `process` on the UI thread: creating and
//! initialising an instance, activation, state, the edit controller and the
//! editor. It is not pedantry. JUCE-based plugins, for one, bind their message
//! manager to the thread that first initialises them; instantiate one on a
//! worker thread and its editor never repaints, because its timers fire on a
//! thread with no message loop.
//!
//! So every non-RT plugin call in this crate goes through [`run`] (blocking)
//! or [`post`] (fire-and-forget). With an executor installed (see
//! [`set_main_thread_executor`]) and a caller on another thread, the job is
//! posted to the UI thread; on the UI thread itself, or with no executor at all
//! (tests, a headless tool), it runs inline.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;
use std::thread::ThreadId;

use crate::Vst3Error;

/// A unit of plugin work for the UI thread.
pub type MainThreadJob = Box<dyn FnOnce() + Send + 'static>;

struct Executor {
    thread: ThreadId,
    post: Box<dyn Fn(MainThreadJob) + Send + Sync>,
}

static EXECUTOR: OnceLock<Executor> = OnceLock::new();

/// Install how plugin work reaches the UI thread. **Call it on the UI thread**,
/// once, before the first [`instantiate`](crate::instantiate); the calling
/// thread is recorded as the UI thread.
///
/// `post` must run the job on that thread soon, without blocking the caller.
/// In Tauri:
///
/// ```ignore
/// // inside `.setup(|app| { ... })`, which runs on the main thread
/// let handle = app.handle().clone();
/// wave_vst3::set_main_thread_executor(move |job| {
///     let _ = handle.run_on_main_thread(job);
/// });
/// ```
///
/// Returns `false` (and changes nothing) if an executor was already installed.
pub fn set_main_thread_executor(
    post: impl Fn(MainThreadJob) + Send + Sync + 'static,
) -> bool {
    EXECUTOR
        .set(Executor {
            thread: std::thread::current().id(),
            post: Box::new(post),
        })
        .is_ok()
}

/// Whether plugin work runs inline on the calling thread: it is the UI thread,
/// or no executor is installed.
pub fn on_main_thread() -> bool {
    EXECUTOR
        .get()
        .is_none_or(|executor| executor.thread == std::thread::current().id())
}

/// Run `job` on the UI thread and wait for its result.
///
/// Must not be called while holding a lock the UI thread may be waiting for:
/// the wait has no timeout, because the UI thread may legitimately be busy for
/// a long time (a licence dialog inside a plugin's `initialize`).
pub(crate) fn run<R: Send + 'static>(
    stage: &'static str,
    job: impl FnOnce() -> R + Send + 'static,
) -> Result<R, Vst3Error> {
    let executor = match EXECUTOR.get() {
        Some(executor) if executor.thread != std::thread::current().id() => executor,
        _ => {
            crate::module::enter_com_apartment();
            return catch_unwind(AssertUnwindSafe(job)).map_err(|_| Vst3Error::Panicked { stage });
        }
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    (executor.post)(Box::new(move || {
        crate::module::enter_com_apartment();
        let _ = tx.send(catch_unwind(AssertUnwindSafe(job)));
    }));
    match rx.recv() {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => Err(Vst3Error::Panicked { stage }),
        Err(_) => Err(Vst3Error::MainThreadUnavailable),
    }
}

/// Run `job` on the UI thread without waiting (inline when already there).
pub(crate) fn post(stage: &'static str, job: impl FnOnce() + Send + 'static) {
    let wrapped = move || {
        crate::module::enter_com_apartment();
        if catch_unwind(AssertUnwindSafe(job)).is_err() {
            log::error!("a VST3 plugin panicked while {stage}");
        }
    };
    match EXECUTOR.get() {
        Some(executor) if executor.thread != std::thread::current().id() => {
            (executor.post)(Box::new(wrapped));
        }
        _ => wrapped(),
    }
}
