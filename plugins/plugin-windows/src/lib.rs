//! `mydaw-plugin-windows` -- native windows for external plugin GUIs.
//!
//! (Directory `plugins/plugin-windows`; the crate is named `mydaw-plugin-windows`
//! rather than `mydaw-plugin-plugin-windows`. It hosts *plugin windows*, not
//! Windows-the-OS.)
//!
//! This is Phase 1 spike 3. The plugin publishes one service,
//! [`PluginWindowService`], which opens the top-level windows that external
//! CLAP (and later VST3) editors are embedded into, and reports what the user
//! does to them.
//!
//! # The architecture, and why it is this one
//!
//! Per the scaffold spec a plugin editor gets its **own native floating window**,
//! not a region of the Tauri webview: a plugin draws with its own toolkit into an
//! `HWND`, which cannot be composited into a WebView2 surface.
//!
//! The interesting question is therefore not "how do we make a window" but "who
//! pumps it". CLAP requires the host to call `gui`, `timer-support` and
//! `on_main_thread` callbacks on **the main thread**, which is the thread Tauri's
//! `tao` event loop already owns and never gives back.
//!
//! MYDAW's answer: **create the plugin windows on tao's own thread and let tao's
//! message loop pump them.** A Win32 message queue belongs to a thread, not to a
//! library. `tao` calls `GetMessageW(hwnd = NULL)` + `DispatchMessageW`, which
//! dispatches to *every* window on the thread, ours and the plugin's child window
//! alike. The host's per-frame work (CLAP timers, `on_main_thread`, draining
//! window events) then runs from Tauri's `RunEvent::MainEventsCleared`, and each
//! open window carries a `WM_TIMER` at [`PUMP_INTERVAL_MS`] whose only job is to
//! wake `GetMessage` so that loop keeps turning while the user is idle.
//!
//! The alternative -- a dedicated plugin-UI thread with its own message loop --
//! would have forced the `!Send` `LoadedClapPlugin` (and therefore CLAP instance
//! lifecycle) onto that thread and split the shell's ownership in two. It is only
//! worth that if the shared-thread design does not work; it does, and
//! `tests/native_window.rs` plus the spike's smoke test prove it rather than
//! assert it.
//!
//! # What this crate deliberately does not do
//! - It knows nothing about CLAP. It hands out an `HWND` and reports resize,
//!   close and DPI changes; `mydaw-plugin-host-clap` does the negotiation.
//! - It does not touch process-wide DPI awareness. Tauri sets per-monitor-v2
//!   before any window exists, and a library that fights the shell over that
//!   setting produces blurry windows on someone else's machine.
//! - It never closes a window on its own. See [`PluginWindowHandle`].

#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod win32;

mod service;

pub use service::{
    PluginWindowError, PluginWindowEvent, PluginWindowHandle, PluginWindowId, PluginWindowService,
    WindowOptions, WindowSize, MAX_WINDOW_SIZE, MIN_WINDOW_SIZE, PUMP_INTERVAL_MS,
};

use std::sync::Arc;

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "plugin-windows";

/// Provides [`PluginWindowService`] to the rest of the kernel.
///
/// The service is created during `register`, which the shell runs on the main
/// thread -- the same thread Tauri's event loop later takes over. That is what
/// makes the service's thread-affinity check meaningful: it pins windows to the
/// thread that will actually pump them.
#[derive(Debug, Default)]
pub struct PluginWindowsPlugin {
    service: Option<Arc<PluginWindowService>>,
}

impl PluginWindowsPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// The service this plugin provided, once registered.
    pub fn service(&self) -> Option<&Arc<PluginWindowService>> {
        self.service.as_ref()
    }
}

impl Plugin for PluginWindowsPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, "Plugin Windows")
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        let service = Arc::new(PluginWindowService::new());
        ctx.services
            .provide(Arc::clone(&service))
            .expect("PluginWindowService provided twice: plugin-windows registered more than once");
        self.service = Some(service);
    }

    fn start(&mut self, _ctx: &KernelCtx) {}

    /// Close anything still open. Runs on the main thread, in reverse dependency
    /// order, before the CLAP host is torn down -- which is the wrong order for a
    /// live editor, so the shell closes GUIs itself first and this is the
    /// backstop for a window whose owner leaked it.
    fn stop(&mut self) {
        if let Some(service) = self.service.take() {
            let closed = service.close_all();
            if closed > 0 {
                log::warn!("plugin-windows closed {closed} window(s) left open at shutdown");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;

    #[test]
    fn boots_and_provides_the_window_service() {
        let mut kernel = Kernel::new();
        kernel
            .add_plugin(Box::new(PluginWindowsPlugin::new()))
            .unwrap();
        kernel.boot().unwrap();

        assert_eq!(kernel.boot_order(), vec![PLUGIN_ID]);
        assert!(kernel.ui_panels().is_empty(), "this plugin has no UI panel");

        let service = kernel
            .ctx()
            .services
            .get::<PluginWindowService>()
            .expect("plugin-windows must provide PluginWindowService");
        assert_eq!(service.open_count(), 0);
        assert!(
            service.on_owner_thread(),
            "the service belongs to the thread that booted the kernel"
        );
    }

    #[test]
    fn stopping_releases_the_service_and_closes_nothing_when_idle() {
        let mut plugin = PluginWindowsPlugin::new();
        let mut ctx = KernelCtx::new();
        plugin.register(&mut ctx);
        assert!(plugin.service().is_some());

        plugin.stop();
        assert!(plugin.service().is_none());
        // Tolerates a second stop, as the kernel contract requires.
        plugin.stop();
    }
}
