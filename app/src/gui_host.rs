//! The main thread's half of the plugin-GUI story.
//!
//! [`ClapGuiHost`] owns the open editor (at most one -- this is a spike) and is
//! driven from Tauri's run loop:
//!
//! ```text
//! RunEvent::MainEventsCleared  ->  ClapGuiHost::tick(&mut clap_fx, &bridge)
//!                                     1. serve queued open/close requests
//!                                     2. pump the open editor (resize, close)
//!                                     3. plugin.tick_main_thread()   (CLAP timers,
//!                                                                     on_main_thread)
//! RunEvent::Exit               ->  ClapGuiHost::shutdown(&mut clap_fx)
//! ```
//!
//! `MainEventsCleared` fires whenever the event loop has drained its queue, and
//! the plugin window's own `WM_TIMER` (see `mydaw-plugin-windows`) keeps waking
//! that loop at ~60 Hz while an editor is open. That is the whole pump: no extra
//! thread, no `ControlFlow` games, and nothing for the webview to starve.

use std::sync::Arc;

use mydaw_plugin_host_clap::gui::{ClapGuiError, ClapGuiSession};
use mydaw_plugin_host_clap::hosting::LoadedClapPlugin;
use mydaw_plugin_windows::PluginWindowService;

use crate::gui_bridge::{reply, ClapGuiBridge, ClapGuiCommand, ClapGuiStatusDto};

/// Message shown when the UI asks for an editor and no plugin is loaded.
///
/// Insertion is still an environment variable in this spike
/// (`MYDAW_CLAP_FX`), so "no plugin" is the normal state, not a failure.
pub const NO_PLUGIN_MESSAGE: &str =
    "no CLAP effect is loaded; set MYDAW_CLAP_FX=<bundle>;<plugin id> and restart";

/// Owns the plugin editor window, on the main thread.
pub struct ClapGuiHost {
    windows: Arc<PluginWindowService>,
    session: Option<ClapGuiSession>,
}

impl ClapGuiHost {
    pub fn new(windows: Arc<PluginWindowService>) -> Self {
        Self {
            windows,
            session: None,
        }
    }

    /// Whether an editor is open.
    pub const fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// The editor window's native handle, if one is open.
    ///
    /// For diagnostics and for the smoke test, which drives the window the way a
    /// user's mouse would (a programmatic `SetWindowPos`) and then checks what the
    /// plugin painted.
    pub fn editor_hwnd(&self) -> Option<isize> {
        self.session.as_ref().map(ClapGuiSession::hwnd)
    }

    /// The editor's negotiated size, if one is open.
    pub fn editor_size(&self) -> Option<(u32, u32)> {
        self.session
            .as_ref()
            .map(|session| (session.size().width, session.size().height))
    }

    /// One pass of the main loop's plugin obligations.
    ///
    /// `plugin` is `None` when no CLAP effect is loaded, in which case requests
    /// are answered with [`NO_PLUGIN_MESSAGE`] rather than ignored.
    pub fn tick(&mut self, plugin: Option<&mut LoadedClapPlugin>, bridge: &ClapGuiBridge) {
        let Some(plugin) = plugin else {
            for (_, channel) in bridge.drain() {
                reply(&channel, Err(NO_PLUGIN_MESSAGE.to_owned()));
            }
            return;
        };

        self.serve_requests(plugin, bridge);
        self.pump_session(plugin);
        // Timers and `on_main_thread` are owed to the plugin whether or not its
        // editor is open: a plugin may schedule main-thread work at any time.
        plugin.tick_main_thread();
        bridge.set_open(self.is_open());
    }

    fn serve_requests(&mut self, plugin: &mut LoadedClapPlugin, bridge: &ClapGuiBridge) {
        for (command, channel) in bridge.drain() {
            let answer = match command {
                ClapGuiCommand::Open => self.open(plugin),
                ClapGuiCommand::Close => {
                    self.close(plugin);
                    Ok(self.status(plugin))
                }
            };
            reply(&channel, answer);
        }
    }

    fn open(&mut self, plugin: &mut LoadedClapPlugin) -> Result<ClapGuiStatusDto, String> {
        if self.session.is_some() {
            // Asking twice is not an error: the editor the user wanted is open.
            return Ok(self.status(plugin));
        }

        let title = format!("{} - MYDAW", plugin.info().name);
        match ClapGuiSession::open(plugin, &self.windows, &title) {
            Ok(session) => {
                log::info!(
                    "plugin editor open: '{}' at {}x{} (scale {})",
                    plugin.info().id,
                    session.size().width,
                    session.size().height,
                    session.scale()
                );
                self.session = Some(session);
                Ok(self.status(plugin))
            }
            Err(err) => {
                // Every variant is something the user can act on (or at least
                // understand), so the message goes through as-is.
                log::warn!("could not open the editor of '{}': {err}", plugin.info().id);
                Err(user_message(&err))
            }
        }
    }

    /// Service an open editor: apply plugin requests, react to the user.
    fn pump_session(&mut self, plugin: &mut LoadedClapPlugin) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let outcome = session.pump(plugin, &self.windows);
        if outcome.close_requested {
            self.close(plugin);
        }
    }

    /// Close the editor, if any. Safe to call repeatedly.
    pub fn close(&mut self, plugin: &mut LoadedClapPlugin) {
        if let Some(session) = self.session.take() {
            session.close(plugin, &self.windows);
            log::info!("plugin editor closed: '{}'", plugin.info().id);
        }
    }

    /// Tear everything down before the plugin is unloaded.
    ///
    /// Order matters and is the reason this exists: a plugin bundle must not be
    /// unloaded while one of its windows is alive, or the next message dispatched
    /// to that window calls into freed code.
    pub fn shutdown(&mut self, plugin: Option<&mut LoadedClapPlugin>) {
        match plugin {
            Some(plugin) => self.close(plugin),
            None => {
                if self.session.is_some() {
                    // Cannot destroy the plugin's GUI without the plugin. Dropping
                    // the session logs loudly; closing the windows is still right.
                    log::error!("plugin editor still open with no plugin to close it against");
                    self.session = None;
                }
            }
        }
        let closed = self.windows.close_all();
        if closed > 0 {
            log::debug!("closed {closed} leftover plugin window(s) at shutdown");
        }
    }

    fn status(&self, plugin: &LoadedClapPlugin) -> ClapGuiStatusDto {
        let plugin_id = Some(plugin.info().id.clone());
        match &self.session {
            Some(session) => ClapGuiStatusDto {
                open: true,
                plugin_id,
                width: session.size().width,
                height: session.size().height,
            },
            None => ClapGuiStatusDto::closed(plugin_id),
        }
    }
}

/// The sentence the status bar shows for a failed open.
///
/// `NoGuiSupport` is the common one and deserves plain language: a plugin with no
/// editor is normal, not broken.
pub fn user_message(error: &ClapGuiError) -> String {
    match error {
        ClapGuiError::NoGuiSupport => "This plugin has no editor.".to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui_bridge::ClapGuiCommand;

    #[test]
    fn without_a_plugin_every_request_is_answered_with_the_reason() {
        let windows = Arc::new(PluginWindowService::new());
        let mut host = ClapGuiHost::new(Arc::clone(&windows));
        let bridge = Arc::new(ClapGuiBridge::new());

        // A request queued by a command thread that has already given up (the
        // realistic shape of a test without a real event loop).
        let queued = Arc::clone(&bridge);
        let asker = std::thread::spawn(move || queued.request(ClapGuiCommand::Open));

        // Wait for it to be queued, then serve it exactly as the run loop would.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while bridge.pending() == 0 {
            assert!(std::time::Instant::now() < deadline, "request never queued");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        host.tick(None, &bridge);

        let answer = asker.join().unwrap();
        assert_eq!(answer.unwrap_err(), NO_PLUGIN_MESSAGE);
        assert!(!host.is_open());
        assert_eq!(windows.open_count(), 0);
    }

    #[test]
    fn ticking_without_a_plugin_or_a_request_does_nothing() {
        let windows = Arc::new(PluginWindowService::new());
        let mut host = ClapGuiHost::new(windows);
        let bridge = ClapGuiBridge::new();

        host.tick(None, &bridge);
        assert!(!host.is_open());
        assert_eq!(bridge.pending(), 0);
        assert!(!bridge.is_open());
    }

    #[test]
    fn shutting_down_with_nothing_open_closes_nothing() {
        let windows = Arc::new(PluginWindowService::new());
        let mut host = ClapGuiHost::new(Arc::clone(&windows));
        host.shutdown(None);
        assert!(!host.is_open());
        assert_eq!(windows.open_count(), 0);
    }

    #[test]
    fn the_no_editor_message_is_plain_language() {
        assert_eq!(
            user_message(&ClapGuiError::NoGuiSupport),
            "This plugin has no editor."
        );
        // Anything else keeps the detail: the user (or the log) needs it.
        let other = ClapGuiError::UnsupportedApi { api: "win32" };
        assert!(user_message(&other).contains("win32"));
    }
}
