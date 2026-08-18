//! `mydaw-plugin-windows` -- native child windows for external plugin GUIs.
//! **Stub.**
//!
//! (Directory `plugins/plugin-windows`; the crate is named `mydaw-plugin-windows`
//! rather than `mydaw-plugin-plugin-windows`. It hosts *plugin windows*, not
//! Windows-the-OS.)
//!
//! Registered from boot day one so the plugin list and the dependency graph show
//! the real shape of the system. Contributes nothing yet.
//!
//! # Planned scope -- Phase 1, spike 3
//! Per `docs/superpowers/specs/2026-08-18-mydaw-scaffold-design.md`: "external
//! plugin GUI in native child window, idle pump, resize/DPI".
//!
//! - Dependency: `tao` (the windowing half of Tauri, so the app links one event
//!   loop rather than two).
//! - A `PluginWindowService` that opens a child window per plugin instance and
//!   hands the raw `HWND` to the CLAP `gui` extension via
//!   `clap_window`/`CLAP_WINDOW_API_WIN32`. VST3 uses `IPlugView::attached` with
//!   the same handle.
//! - **The idle pump is the actual risk.** CLAP requires the host to call the
//!   plugin's `on_timer`/`gui` callbacks on the *main* thread at a steady rate,
//!   while Tauri owns that thread's event loop. The spike measures whether a
//!   `tao` user event / `ControlFlow::WaitUntil` tick keeps plugin editors
//!   responsive without starving the webview.
//! - Per-monitor-v2 DPI and resize negotiation (`gui.can_resize`,
//!   `gui.adjust_size`) are the second half of the spike: getting these wrong
//!   produces plugin editors that are blurry or clipped on mixed-DPI setups.
//!
//! Nothing above is implemented yet. Deliberately.

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "plugin-windows";

/// Native child windows for external plugin editors. Contributes nothing until
/// Phase 1 spike 3.
#[derive(Debug, Default, Clone, Copy)]
pub struct PluginWindowsPlugin;

impl PluginWindowsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for PluginWindowsPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, "Plugin Windows")
    }

    /// No contributions yet. See the crate docs for the planned scope.
    fn register(&mut self, _ctx: &mut KernelCtx) {}

    fn start(&mut self, _ctx: &KernelCtx) {}

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;

    #[test]
    fn boots_and_contributes_nothing() {
        let mut kernel = Kernel::new();
        kernel
            .add_plugin(Box::new(PluginWindowsPlugin::new()))
            .unwrap();
        kernel.boot().unwrap();

        assert_eq!(kernel.boot_order(), vec![PLUGIN_ID]);
        assert!(kernel.ui_panels().is_empty());
        assert!(kernel.ctx().services.is_empty());
    }
}
