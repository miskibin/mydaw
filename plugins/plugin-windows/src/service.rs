//! [`PluginWindowService`] -- the portable half of the window manager.
//!
//! Everything here is platform-independent: identity, sizing rules, the event
//! channel, thread-affinity enforcement and the bookkeeping of which windows are
//! open. The Win32 calls that actually make a window exist live in
//! [`crate::win32`], behind a handful of free functions, so that this file can be
//! read (and most of it tested) without a display.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::ThreadId;

use parking_lot::Mutex;

#[cfg(windows)]
use crate::win32 as backend;

#[cfg(not(windows))]
use crate::unsupported as backend;

/// Smallest client area a plugin window may be given.
///
/// A plugin that asks for less gets this instead: a 1x1 window is not a window,
/// it is a way to lose the close button.
pub const MIN_WINDOW_SIZE: WindowSize = WindowSize {
    width: 120,
    height: 80,
};

/// Largest client area. Bigger than any real editor, and small enough that a
/// plugin returning garbage cannot ask the compositor for a gigabyte of surface.
pub const MAX_WINDOW_SIZE: WindowSize = WindowSize {
    width: 8192,
    height: 8192,
};

/// Period of the wake-up timer each plugin window carries, in milliseconds.
///
/// The timer's only job is to make the host's message loop iterate: `WM_TIMER`
/// unblocks `GetMessage`, which is what gives the host a chance to service CLAP
/// timers and `on_main_thread` callbacks while the user is doing nothing.
/// ~60 Hz, which is also the rate a plugin editor wants to animate at.
pub const PUMP_INTERVAL_MS: u32 = 16;

/// Size of a window's **client area**, in physical pixels.
///
/// Physical, not logical, because that is what `CLAP_WINDOW_API_WIN32` specifies:
/// the plugin is told a scale factor separately and is expected to work in device
/// pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl WindowSize {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// This size pulled inside [`MIN_WINDOW_SIZE`]..=[`MAX_WINDOW_SIZE`].
    ///
    /// Both bounds are enforced because both are reachable from plugin-controlled
    /// data: `clap_plugin_gui->get_size()` is whatever the vendor wrote.
    #[must_use]
    pub const fn clamped(self) -> Self {
        Self {
            width: clamp_u32(self.width, MIN_WINDOW_SIZE.width, MAX_WINDOW_SIZE.width),
            height: clamp_u32(self.height, MIN_WINDOW_SIZE.height, MAX_WINDOW_SIZE.height),
        }
    }

    /// Whether this size is already within the limits.
    pub const fn is_within_limits(self) -> bool {
        self.width >= MIN_WINDOW_SIZE.width
            && self.height >= MIN_WINDOW_SIZE.height
            && self.width <= MAX_WINDOW_SIZE.width
            && self.height <= MAX_WINDOW_SIZE.height
    }
}

/// `u32::clamp` is not `const` in this toolchain; this is.
const fn clamp_u32(value: u32, low: u32, high: u32) -> u32 {
    if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }
}

/// Handle-identity of an open window. Never reused within a process run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginWindowId(u64);

impl PluginWindowId {
    /// The raw counter value, for logs and tests.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Construct an id that no service ever handed out. Tests only: it lets the
    /// unknown-window error paths be exercised without opening a window.
    #[cfg(test)]
    pub(crate) const fn for_test(value: u64) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for PluginWindowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "window#{}", self.0)
    }
}

/// What a plugin window tells its owner.
///
/// Delivered through an `mpsc` channel filled by the window procedure, which runs
/// inside the host's `DispatchMessage`. The owner drains it from its own loop, so
/// no host code runs *inside* a window procedure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginWindowEvent {
    /// The user clicked the close button. The window is still alive: closing is
    /// the owner's decision, because a CLAP GUI must be destroyed before the
    /// window it is embedded in.
    Closed,
    /// The client area changed size (user drag, DPI change, or a programmatic
    /// resize).
    Resized(WindowSize),
    /// The window moved to a monitor with a different DPI. The window has already
    /// been moved and resized to the suggested rectangle; the client size that
    /// results arrives as a separate [`Resized`](Self::Resized).
    DpiChanged { dpi: u32 },
}

/// Everything that can go wrong opening or driving a plugin window.
#[derive(Debug, thiserror::Error)]
pub enum PluginWindowError {
    /// A window call was made from a thread other than the one that created the
    /// service. Win32 pins a window to its creating thread: its messages are only
    /// ever delivered to that thread's queue, and `DestroyWindow` from another
    /// thread simply fails.
    #[error("plugin windows are owned by thread {owner:?}; this call came from {caller:?}")]
    WrongThread { owner: ThreadId, caller: ThreadId },

    /// This platform has no plugin-window backend yet (everything but Windows).
    #[error("native plugin windows are not implemented on this platform")]
    Unsupported,

    /// `RegisterClassW` failed. Reports `GetLastError`.
    #[error("could not register the plugin window class (win32 error {0})")]
    ClassRegistration(u32),

    /// `CreateWindowExW` failed. Reports `GetLastError`.
    #[error("could not create the plugin window (win32 error {0})")]
    Creation(u32),

    /// The id does not belong to a window this service still owns.
    #[error("{0} is not open")]
    UnknownWindow(PluginWindowId),

    /// A Win32 call on a live window failed. Reports the API and `GetLastError`.
    #[error("{api} failed on {id} (win32 error {code})")]
    Win32 {
        id: PluginWindowId,
        api: &'static str,
        code: u32,
    },
}

/// How a window should be created.
#[derive(Debug, Clone)]
pub struct WindowOptions {
    /// Title bar text.
    pub title: String,
    /// Requested client size. Clamped to the service's limits.
    pub size: WindowSize,
    /// Whether the user may drag the frame. `false` produces a fixed-size window
    /// (no thick frame, no maximise box), which is what a plugin reporting
    /// `can_resize() == false` requires: otherwise the user drags a frame the
    /// plugin will refuse to follow.
    pub resizable: bool,
}

impl WindowOptions {
    pub fn new(title: impl Into<String>, size: WindowSize) -> Self {
        Self {
            title: title.into(),
            size,
            resizable: false,
        }
    }

    #[must_use]
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }
}

/// An open window, from its owner's point of view.
///
/// Deliberately **not** `Send`: the raw `HWND` inside is only meaningful on the
/// thread that created it. Dropping this handle does *not* close the window --
/// the service owns the window; call [`PluginWindowService::close_window`].
/// (A CLAP GUI must be destroyed before the window it is embedded in, so an
/// implicit close-on-drop would be a use-after-free waiting for a vendor to find
/// it.)
pub struct PluginWindowHandle {
    id: PluginWindowId,
    hwnd: isize,
    events: Receiver<PluginWindowEvent>,
    ticks: Arc<AtomicU64>,
    /// Makes the handle `!Send`/`!Sync`. See the type docs.
    _thread_bound: std::marker::PhantomData<*const ()>,
}

impl PluginWindowHandle {
    pub const fn id(&self) -> PluginWindowId {
        self.id
    }

    /// The raw `HWND` as an integer, for handing to a plugin's `clap_window`.
    ///
    /// Valid only until [`PluginWindowService::close_window`] is called, and only
    /// on the owning thread.
    pub const fn hwnd(&self) -> isize {
        self.hwnd
    }

    /// The event channel. Drain it from the owner's loop.
    pub const fn events(&self) -> &Receiver<PluginWindowEvent> {
        &self.events
    }

    /// Every event queued since the last drain.
    ///
    /// Non-blocking. Returns an empty vector when nothing happened, which is the
    /// normal case at 60 Hz.
    pub fn drain_events(&self) -> Vec<PluginWindowEvent> {
        self.events.try_iter().collect()
    }

    /// `WM_TIMER` ticks this window's wake-up timer has delivered.
    ///
    /// This is the observable proof that the host's message loop dispatches to
    /// windows it did not create -- the premise the whole spike rests on.
    pub fn pump_ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for PluginWindowHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginWindowHandle")
            .field("id", &self.id)
            .field("hwnd", &format_args!("{:#x}", self.hwnd))
            .field("pump_ticks", &self.pump_ticks())
            .finish()
    }
}

/// One open window, as the service tracks it.
struct OpenWindow {
    hwnd: isize,
}

/// Native windows for external plugin editors.
///
/// # Thread affinity
/// Every method that touches a window must run on the thread that created the
/// service, and that thread must be the one running the application's message
/// loop -- in MYDAW, Tauri's. Win32 delivers a window's messages only to its
/// creating thread's queue, and `GetMessage`/`DispatchMessage` there dispatches
/// to *every* window on the thread regardless of who created it. That is what
/// lets these windows live inside tao's loop with no integration at all.
///
/// Calls from another thread return [`PluginWindowError::WrongThread`] rather
/// than silently doing nothing, which is what the raw API would do.
pub struct PluginWindowService {
    owner: ThreadId,
    next_id: AtomicU64,
    windows: Mutex<Vec<(PluginWindowId, OpenWindow)>>,
}

impl Default for PluginWindowService {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginWindowService {
    /// Create a service owned by the calling thread.
    pub fn new() -> Self {
        Self {
            owner: std::thread::current().id(),
            next_id: AtomicU64::new(1),
            windows: Mutex::new(Vec::new()),
        }
    }

    /// The thread this service (and every window it opens) belongs to.
    pub fn owner_thread(&self) -> ThreadId {
        self.owner
    }

    /// Whether the caller is on the owning thread.
    pub fn on_owner_thread(&self) -> bool {
        std::thread::current().id() == self.owner
    }

    fn check_thread(&self) -> Result<(), PluginWindowError> {
        if self.on_owner_thread() {
            Ok(())
        } else {
            Err(PluginWindowError::WrongThread {
                owner: self.owner,
                caller: std::thread::current().id(),
            })
        }
    }

    /// Open a hidden top-level window.
    ///
    /// Hidden on purpose: CLAP's embedding sequence is create GUI -> set parent ->
    /// show, and a host window that flashes empty before the plugin has drawn in
    /// it looks like a bug. Call [`show_window`](Self::show_window) once the
    /// plugin has been parented.
    ///
    /// # Errors
    /// [`PluginWindowError::WrongThread`] off the owning thread,
    /// [`PluginWindowError::Unsupported`] on a platform without a backend, and
    /// [`PluginWindowError::ClassRegistration`] / [`PluginWindowError::Creation`]
    /// when Win32 refuses.
    pub fn open_window(
        &self,
        options: &WindowOptions,
    ) -> Result<PluginWindowHandle, PluginWindowError> {
        self.check_thread()?;

        let size = options.size.clamped();
        let (sender, receiver) = channel();
        let ticks = Arc::new(AtomicU64::new(0));
        let hwnd = backend::create_window(options, size, sender, Arc::clone(&ticks))?;

        let id = PluginWindowId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.windows.lock().push((id, OpenWindow { hwnd }));
        log::debug!(
            "opened plugin {id} '{}' ({}x{}) hwnd {hwnd:#x}",
            options.title,
            size.width,
            size.height
        );

        Ok(PluginWindowHandle {
            id,
            hwnd,
            events: receiver,
            ticks,
            _thread_bound: std::marker::PhantomData,
        })
    }

    /// Destroy a window. A second call reports
    /// [`PluginWindowError::UnknownWindow`] rather than doing damage.
    ///
    /// # Errors
    /// [`PluginWindowError::WrongThread`], [`PluginWindowError::UnknownWindow`].
    pub fn close_window(&self, id: PluginWindowId) -> Result<(), PluginWindowError> {
        self.check_thread()?;
        let hwnd = self.take(id).ok_or(PluginWindowError::UnknownWindow(id))?;
        backend::destroy_window(hwnd);
        log::debug!("closed plugin {id}");
        Ok(())
    }

    /// Make a window visible and bring it to the front.
    ///
    /// # Errors
    /// [`PluginWindowError::WrongThread`], [`PluginWindowError::UnknownWindow`].
    pub fn show_window(&self, id: PluginWindowId) -> Result<(), PluginWindowError> {
        self.check_thread()?;
        let hwnd = self.hwnd_of(id)?;
        backend::show_window(hwnd);
        Ok(())
    }

    /// Resize the **client area**, keeping the top-left corner where it is.
    ///
    /// # Errors
    /// [`PluginWindowError::WrongThread`], [`PluginWindowError::UnknownWindow`],
    /// [`PluginWindowError::Win32`].
    pub fn set_client_size(
        &self,
        id: PluginWindowId,
        size: WindowSize,
    ) -> Result<(), PluginWindowError> {
        self.check_thread()?;
        let hwnd = self.hwnd_of(id)?;
        backend::set_client_size(id, hwnd, size.clamped())
    }

    /// Current client-area size.
    ///
    /// # Errors
    /// [`PluginWindowError::WrongThread`], [`PluginWindowError::UnknownWindow`],
    /// [`PluginWindowError::Win32`].
    pub fn client_size(&self, id: PluginWindowId) -> Result<WindowSize, PluginWindowError> {
        self.check_thread()?;
        let hwnd = self.hwnd_of(id)?;
        backend::client_size(id, hwnd)
    }

    /// The window's current DPI (96 = 100%).
    ///
    /// # Errors
    /// [`PluginWindowError::WrongThread`], [`PluginWindowError::UnknownWindow`].
    pub fn dpi(&self, id: PluginWindowId) -> Result<u32, PluginWindowError> {
        self.check_thread()?;
        let hwnd = self.hwnd_of(id)?;
        Ok(backend::dpi(hwnd))
    }

    /// Whether anything has embedded a child window inside this window.
    ///
    /// The host uses it to notice a plugin that reported a successful
    /// `set_parent` without actually embedding anything -- which, with
    /// `clack-extensions` 0.1.1, is what a *failed* `set_parent` looks like from
    /// the host's side (its FFI bridge discards the plugin's return value).
    ///
    /// # Errors
    /// [`PluginWindowError::WrongThread`], [`PluginWindowError::UnknownWindow`],
    /// and [`PluginWindowError::Unsupported`] on a platform with no backend --
    /// which callers should treat as "cannot tell", not as "nothing embedded".
    pub fn has_child_window(&self, id: PluginWindowId) -> Result<bool, PluginWindowError> {
        self.check_thread()?;
        let hwnd = self.hwnd_of(id)?;
        backend::has_child_window(hwnd)
    }

    /// Whether this service still owns a window with that id.
    pub fn is_open(&self, id: PluginWindowId) -> bool {
        self.windows.lock().iter().any(|(known, _)| *known == id)
    }

    /// How many windows are open.
    pub fn open_count(&self) -> usize {
        self.windows.lock().len()
    }

    /// Destroy every open window. Returns how many were closed.
    ///
    /// Called from the plugin's `stop()`, on the owning thread. Off-thread it
    /// closes nothing and says so in the log rather than corrupting the registry.
    pub fn close_all(&self) -> usize {
        if !self.on_owner_thread() {
            if self.open_count() > 0 {
                log::warn!(
                    "PluginWindowService::close_all called off the owning thread; \
                     {} window(s) left open",
                    self.open_count()
                );
            }
            return 0;
        }
        let open: Vec<(PluginWindowId, OpenWindow)> = std::mem::take(&mut *self.windows.lock());
        let count = open.len();
        for (_, window) in open {
            backend::destroy_window(window.hwnd);
        }
        count
    }

    fn hwnd_of(&self, id: PluginWindowId) -> Result<isize, PluginWindowError> {
        self.windows
            .lock()
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, window)| window.hwnd)
            .ok_or(PluginWindowError::UnknownWindow(id))
    }

    fn take(&self, id: PluginWindowId) -> Option<isize> {
        let mut windows = self.windows.lock();
        let index = windows.iter().position(|(known, _)| *known == id)?;
        Some(windows.remove(index).1.hwnd)
    }
}

impl std::fmt::Debug for PluginWindowService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginWindowService")
            .field("owner", &self.owner)
            .field("open", &self.open_count())
            .finish()
    }
}

impl Drop for PluginWindowService {
    fn drop(&mut self) {
        let leaked = self.close_all();
        if leaked > 0 {
            log::debug!("PluginWindowService dropped with {leaked} window(s) still open");
        }
    }
}

/// State every window procedure needs. Boxed and owned by the window itself.
pub(crate) struct WindowState {
    pub(crate) events: Sender<PluginWindowEvent>,
    pub(crate) ticks: Arc<AtomicU64>,
    pub(crate) min_size: WindowSize,
}

impl WindowState {
    /// Post an event to the owner, ignoring a disconnected channel.
    ///
    /// A dropped receiver is not an error: the owner may have released its handle
    /// while the window is still being torn down, and a window procedure has
    /// nowhere useful to report to anyway.
    pub(crate) fn post(&self, event: PluginWindowEvent) {
        let _ = self.events.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_clamped_into_the_supported_range() {
        assert_eq!(
            WindowSize::new(400, 250).clamped(),
            WindowSize::new(400, 250)
        );
        assert_eq!(WindowSize::new(0, 0).clamped(), MIN_WINDOW_SIZE);
        assert_eq!(
            WindowSize::new(u32::MAX, u32::MAX).clamped(),
            MAX_WINDOW_SIZE
        );
        // Mixed: one axis too small, the other too large.
        assert_eq!(
            WindowSize::new(1, 100_000).clamped(),
            WindowSize::new(MIN_WINDOW_SIZE.width, MAX_WINDOW_SIZE.height)
        );
        assert!(WindowSize::new(400, 250).is_within_limits());
        assert!(!WindowSize::new(400, 1).is_within_limits());
    }

    #[test]
    fn ids_are_unique_and_never_reused() {
        let service = PluginWindowService::new();
        let first = PluginWindowId(service.next_id.fetch_add(1, Ordering::Relaxed));
        let second = PluginWindowId(service.next_id.fetch_add(1, Ordering::Relaxed));
        assert_ne!(first, second);
        assert_eq!(second.get(), first.get() + 1);
        assert_eq!(first.to_string(), "window#1");
    }

    #[test]
    fn a_service_starts_empty_and_knows_its_thread() {
        let service = PluginWindowService::new();
        assert_eq!(service.open_count(), 0);
        assert!(service.on_owner_thread());
        assert_eq!(service.owner_thread(), std::thread::current().id());
        assert!(!service.is_open(PluginWindowId::for_test(1)));
    }

    #[test]
    fn window_calls_from_another_thread_are_refused() {
        let service = Arc::new(PluginWindowService::new());
        let elsewhere = Arc::clone(&service);
        let outcome = std::thread::spawn(move || {
            let id = PluginWindowId::for_test(1);
            let opened = elsewhere.open_window(&WindowOptions::new("x", WindowSize::new(200, 100)));
            (
                opened.err(),
                elsewhere.close_window(id).err(),
                elsewhere
                    .set_client_size(id, WindowSize::new(200, 100))
                    .err(),
                elsewhere.client_size(id).err(),
                elsewhere.dpi(id).err(),
                elsewhere.show_window(id).err(),
                elsewhere.on_owner_thread(),
            )
        })
        .join()
        .expect("the probe thread must not panic");

        for err in [
            outcome.0, outcome.1, outcome.2, outcome.3, outcome.4, outcome.5,
        ] {
            assert!(
                matches!(err, Some(PluginWindowError::WrongThread { .. })),
                "off-thread calls must be refused, got {err:?}"
            );
        }
        assert!(!outcome.6);
    }

    #[test]
    fn closing_an_unknown_window_is_a_typed_error() {
        let service = PluginWindowService::new();
        let id = PluginWindowId::for_test(42);
        assert!(matches!(
            service.close_window(id),
            Err(PluginWindowError::UnknownWindow(unknown)) if unknown == id
        ));
        assert!(matches!(
            service.client_size(id),
            Err(PluginWindowError::UnknownWindow(_))
        ));
        assert!(matches!(
            service.show_window(id),
            Err(PluginWindowError::UnknownWindow(_))
        ));
    }

    #[test]
    fn events_reach_the_owner_through_the_channel() {
        // The window procedure's half of the contract, without a window: the
        // state posts, the handle drains, a dropped receiver is not fatal.
        let (sender, receiver) = channel();
        let ticks = Arc::new(AtomicU64::new(0));
        let state = WindowState {
            events: sender,
            ticks: Arc::clone(&ticks),
            min_size: MIN_WINDOW_SIZE,
        };
        let handle = PluginWindowHandle {
            id: PluginWindowId::for_test(7),
            hwnd: 0,
            events: receiver,
            ticks,
            _thread_bound: std::marker::PhantomData,
        };

        assert!(handle.drain_events().is_empty());
        state.post(PluginWindowEvent::Resized(WindowSize::new(640, 480)));
        state.post(PluginWindowEvent::DpiChanged { dpi: 144 });
        state.post(PluginWindowEvent::Closed);
        assert_eq!(
            handle.drain_events(),
            vec![
                PluginWindowEvent::Resized(WindowSize::new(640, 480)),
                PluginWindowEvent::DpiChanged { dpi: 144 },
                PluginWindowEvent::Closed,
            ]
        );
        assert!(handle.drain_events().is_empty());

        state.ticks.fetch_add(3, Ordering::Relaxed);
        assert_eq!(handle.pump_ticks(), 3);

        drop(handle);
        // No panic, no error: the window outlives its owner's interest.
        state.post(PluginWindowEvent::Closed);
    }

    #[test]
    fn options_default_to_fixed_size() {
        let options = WindowOptions::new("Editor", WindowSize::new(400, 250));
        assert!(!options.resizable);
        assert!(options.clone().resizable(true).resizable);
        assert_eq!(options.title, "Editor");
    }
}
