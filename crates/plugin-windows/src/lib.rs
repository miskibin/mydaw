//! `wave-plugin-windows` -- native top-level windows for plugin editors.
//!
//! A VST3 editor paints with its own toolkit into a native window handle
//! (`HWND` on Windows). That cannot be composited into the Tauri webview, so
//! every editor gets its **own floating native window** next to WAVE's, and the
//! plugin's child window is embedded in it.
//!
//! # Who pumps the windows
//!
//! A Win32 message queue belongs to a *thread*, not to a library. Tauri's event
//! loop (tao) runs `GetMessageW(hwnd = NULL)` + `DispatchMessageW` on the main
//! thread, which dispatches to **every** window created on that thread. So the
//! windows made here, and the child window the plugin creates inside them, are
//! pumped by Tauri's own loop with no integration at all -- *provided they are
//! created on that thread*. That is the one rule of this crate, and
//! [`Service`] enforces it: every call that touches a window checks the calling
//! thread and returns [`WindowError::WrongThread`] otherwise (the raw API would
//! silently do nothing).
//!
//! Each window carries a `WM_TIMER` at [`PUMP_INTERVAL_MS`]. It keeps the loop
//! turning while the user is idle, and it is when the window's owner gets its
//! *tick* ([`Window::set_tick_handler`]): resize requests from the plugin, user
//! drags, DPI changes and the close button are serviced there, on the UI thread,
//! never from inside a nested window-procedure call.
//!
//! # How the app uses it
//!
//! ```text
//! main thread (Tauri setup)        let windows = wave_plugin_windows::Service::new();
//!                                  app.manage(windows);            // Service is Send + Sync
//! any thread (a Tauri command)     app.run_on_main_thread(move || {
//!                                      let windows = app.state::<Service>();
//!                                      handle.open_editor(&windows)   // wave-vst3
//!                                  });
//! main thread (RunEvent::Exit)     windows.close_all();   // before audio stops and plugins drop
//! ```
//!
//! Nothing else: there is no per-frame call to make. [`Service::close_all`]
//! first *asks* every window's owner to close (so an editor is detached from
//! its window before the window dies) and destroys whatever is left.
//!
//! # Platforms
//!
//! Only Windows has a backend. Everywhere else the crate compiles and every
//! window call returns [`WindowError::Unsupported`], so callers can be written
//! once.
//!
//! # What this crate deliberately does not do
//! - It knows nothing about VST3. It hands out a native handle and reports
//!   resize, close and DPI changes; `wave-vst3` does the negotiation.
//! - It does not touch process-wide DPI awareness. Tauri sets per-monitor-v2
//!   before any window exists, and a library that fights the shell over that
//!   produces blurry windows.

#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod win32;

mod service;

pub use service::{
    Service, Window, WindowError, WindowEvent, WindowId, WindowOptions, WindowSize,
    MAX_WINDOW_SIZE, MIN_WINDOW_SIZE, PUMP_INTERVAL_MS,
};

/// Whether this build has a native window backend. `false` means every
/// [`Service::open_window`] returns [`WindowError::Unsupported`].
pub const SUPPORTED: bool = cfg!(windows);
