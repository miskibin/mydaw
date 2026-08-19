//! Showing a CLAP plugin's editor in a native MYDAW window.
//!
//! This is Phase 1 spike 3. [`hosting`](crate::hosting) makes a plugin *process
//! audio*; this module makes it *visible*.
//!
//! # The sequence, and why it is in this order
//!
//! CLAP's `gui` extension is a protocol, not a set of independent calls. The one
//! [`ClapGuiSession::open`] performs is the embedded-window sequence from the
//! specification, with the DPI step that Win32 forces on top:
//!
//! ```text
//!  is_api_supported(win32, embedded)   is this plugin willing at all?
//!  create(win32, floating = false)     plugin allocates its editor
//!  can_resize()                        decides whether our frame is draggable
//!  get_size()                          the size to make the window
//!  --- host opens the window here ---
//!  set_scale(dpi / 96)                 only now is the monitor's DPI known
//!  get_size()                          may have changed with the scale
//!  set_parent(HWND)                    plugin re-parents its child window
//!  show()                              plugin makes its editor visible
//!  --- host shows the window here ---
//! ```
//!
//! The window is opened *hidden* and shown last, so the user never sees an empty
//! frame waiting for a plugin to draw into it.
//!
//! # Resizing is a negotiation
//!
//! A user dragging the frame is a *request*. The plugin gets `adjust_size` first
//! and may round it down (aspect ratios, tile grids, fixed pixel art); only then
//! is `set_size` called and the window corrected to what was agreed. Skipping the
//! negotiation and calling `set_size` with a raw drag size is the classic host
//! bug that leaves editors clipped -- [`decide_resize`] is that rule written down
//! and tested on its own, without a window in sight.
//!
//! A plugin can also ask the other way (`clap_host_gui->request_resize`), which
//! may arrive from any thread; it is latched in
//! [`ClapGuiRequests`](crate::hosting::ClapGuiRequests) and applied here on the
//! main thread.
//!
//! # Threading
//! Everything in this module is `[main-thread]`. It takes `&mut LoadedClapPlugin`
//! (which is `!Send`) and a `&PluginWindowService` (which refuses calls from
//! other threads), so there is no way to get it wrong by accident.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiError, GuiSize, PluginGui, Window};
use mydaw_plugin_windows::{
    PluginWindowError, PluginWindowEvent, PluginWindowHandle, PluginWindowId, PluginWindowService,
    WindowOptions, WindowSize,
};

use crate::hosting::LoadedClapPlugin;

/// Size used when a plugin claims an editor but will not say how big it is.
/// A CLAP plugin is required to answer `get_size`; this is what a broken one gets
/// instead of a zero-pixel window.
pub const FALLBACK_SIZE: GuiSize = GuiSize {
    width: 400,
    height: 300,
};

/// Reference DPI: 96 is 100% scaling on Windows.
pub const REFERENCE_DPI: u32 = 96;

/// The GUI scale a plugin should be told about at this DPI.
pub fn scale_for_dpi(dpi: u32) -> f64 {
    if dpi == 0 {
        1.0
    } else {
        f64::from(dpi) / f64::from(REFERENCE_DPI)
    }
}

/// Everything that can go wrong between "the user clicked Open GUI" and "the
/// editor is on screen".
#[derive(Debug, thiserror::Error)]
pub enum ClapGuiError {
    /// The plugin does not implement `clap.gui` at all. Common and not an error
    /// in the plugin: plenty of effects have no editor.
    #[error("this plugin has no editor (it does not implement the CLAP 'gui' extension)")]
    NoGuiSupport,

    /// The plugin implements `gui` but not in a way this host can drive --
    /// typically an editor that only supports floating windows on a platform
    /// where MYDAW embeds.
    #[error("this plugin's editor does not support the embedded {api} windowing API")]
    UnsupportedApi { api: &'static str },

    /// A `clap_plugin_gui` call returned failure.
    #[error("the plugin refused while {stage}: {source}")]
    Gui {
        stage: &'static str,
        #[source]
        source: GuiError,
    },

    /// The native window could not be created or driven.
    #[error("the plugin window could not be opened: {0}")]
    Window(#[from] PluginWindowError),

    /// Plugin code panicked through the FFI boundary while its editor was being
    /// set up. The session is abandoned; the plugin keeps processing audio.
    #[error("the plugin panicked while {stage}")]
    Panicked { stage: &'static str },
}

impl ClapGuiError {
    fn gui(stage: &'static str, source: GuiError) -> Self {
        Self::Gui { stage, source }
    }
}

/// What the host should do about a resize request.
///
/// Separated from the window and the plugin so the rule can be tested on its own:
/// the interesting cases (a plugin that rounds sizes, a plugin that cannot resize
/// at all, a resize the host itself just caused) are all decided here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeDecision {
    /// The window is already the size the plugin agreed to. Doing nothing is
    /// important: this is the echo of the host's own `SetWindowPos`, and acting
    /// on it would start a resize loop.
    Ignore,
    /// The plugin cannot be resized. Put the window back where it was.
    RestoreWindow(GuiSize),
    /// Tell the plugin to take `size`.
    Resize {
        size: GuiSize,
        /// Whether the window must be corrected too, i.e. the plugin adjusted the
        /// requested size rather than accepting it.
        correct_window: bool,
    },
}

/// Decide what a new window size means for the plugin.
///
/// - `current` is the size the plugin last agreed to.
/// - `requested` is what the window has just become (a user drag, usually).
/// - `adjusted` is the plugin's answer to `adjust_size(requested)`, or `None`
///   when the plugin does not implement it (in which case the request stands).
pub fn decide_resize(
    current: GuiSize,
    requested: GuiSize,
    can_resize: bool,
    adjusted: Option<GuiSize>,
) -> ResizeDecision {
    if requested == current {
        return ResizeDecision::Ignore;
    }
    if !can_resize {
        return ResizeDecision::RestoreWindow(current);
    }
    let size = adjusted.unwrap_or(requested);
    if size == current {
        // The plugin rounded the drag back to the size it already has: the
        // plugin needs no call, but the window is now wrong.
        return ResizeDecision::RestoreWindow(current);
    }
    ResizeDecision::Resize {
        size,
        correct_window: size != requested,
    }
}

/// What one [`ClapGuiSession::pump`] pass observed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ClapGuiPump {
    /// The user closed the window, or the plugin asked to be closed. The owner
    /// should call [`ClapGuiSession::close`].
    pub close_requested: bool,
    /// The size the editor ended up at, if it changed during this pass.
    pub resized_to: Option<GuiSize>,
}

impl LoadedClapPlugin {
    /// Whether this plugin implements `clap.gui` at all.
    ///
    /// Cheap (one extension query) and answered without creating anything, so
    /// the UI can decide whether to draw an "Open editor" button rather than
    /// drawing one that always fails. A plugin with no editor is perfectly
    /// normal -- plenty of effects have none.
    ///
    /// `[main-thread]`, like every extension query.
    pub fn has_editor(&mut self) -> bool {
        self.plugin_handle().get_extension::<PluginGui>().is_some()
    }
}

/// An open plugin editor: the plugin's `gui` extension plus the window it lives
/// in.
///
/// Not `Send`: it holds a [`PluginWindowHandle`], and every call it makes is
/// `[main-thread]`.
pub struct ClapGuiSession {
    gui: PluginGui,
    window: PluginWindowHandle,
    /// The size the plugin last agreed to.
    size: GuiSize,
    can_resize: bool,
    scale: f64,
    /// Set once the plugin's GUI has been destroyed, so [`close`](Self::close)
    /// and `Drop` cannot do it twice.
    destroyed: bool,
}

impl ClapGuiSession {
    /// Open the plugin's editor in a new native window.
    ///
    /// # Errors
    /// [`ClapGuiError::NoGuiSupport`] when the plugin has no editor (the common
    /// case, and not a failure of anything), plus every way the plugin or the
    /// window system can refuse.
    pub fn open(
        plugin: &mut LoadedClapPlugin,
        windows: &PluginWindowService,
        title: &str,
    ) -> Result<Self, ClapGuiError> {
        catch_unwind(AssertUnwindSafe(|| {
            Self::open_inner(plugin, windows, title)
        }))
        .map_err(|_| ClapGuiError::Panicked {
            stage: "opening its editor",
        })?
    }

    fn open_inner(
        plugin: &mut LoadedClapPlugin,
        windows: &PluginWindowService,
        title: &str,
    ) -> Result<Self, ClapGuiError> {
        let api =
            GuiApiType::default_for_current_platform().ok_or(ClapGuiError::UnsupportedApi {
                api: "(none for this platform)",
            })?;
        let configuration = GuiConfiguration {
            api_type: api,
            is_floating: false,
        };

        let gui = plugin
            .plugin_handle()
            .get_extension::<PluginGui>()
            .ok_or(ClapGuiError::NoGuiSupport)?;

        if !gui.is_api_supported(&mut plugin.plugin_handle(), configuration) {
            return Err(ClapGuiError::UnsupportedApi { api: api_name(api) });
        }

        // Stale flags from a previous session must not be applied to this one.
        plugin.gui_requests().clear();

        gui.create(&mut plugin.plugin_handle(), configuration)
            .map_err(|err| ClapGuiError::gui("creating its editor", err))?;

        // Past this point the plugin holds GUI resources, so every failure has to
        // give them back.
        match Self::embed(plugin, windows, gui, title) {
            Ok(session) => Ok(session),
            Err(err) => {
                gui.destroy(&mut plugin.plugin_handle());
                plugin.gui_requests().clear();
                Err(err)
            }
        }
    }

    fn embed(
        plugin: &mut LoadedClapPlugin,
        windows: &PluginWindowService,
        gui: PluginGui,
        title: &str,
    ) -> Result<Self, ClapGuiError> {
        let can_resize = gui.can_resize(&mut plugin.plugin_handle());
        let size = gui
            .get_size(&mut plugin.plugin_handle())
            .unwrap_or(FALLBACK_SIZE);

        let options = WindowOptions::new(title, to_window_size(size)).resizable(can_resize);
        let window = windows.open_window(&options)?;

        match Self::attach(plugin, windows, gui, &window, size) {
            Ok((size, scale)) => Ok(Self {
                gui,
                window,
                size,
                can_resize,
                scale,
                destroyed: false,
            }),
            Err(err) => {
                // The window exists but the plugin never got into it.
                let _ = windows.close_window(window.id());
                Err(err)
            }
        }
    }

    /// The half of the sequence that needs a live window: scale, size, parent,
    /// show. Returns the size finally agreed and the scale the plugin was told.
    fn attach(
        plugin: &mut LoadedClapPlugin,
        windows: &PluginWindowService,
        gui: PluginGui,
        window: &PluginWindowHandle,
        initial_size: GuiSize,
    ) -> Result<(GuiSize, f64), ClapGuiError> {
        let dpi = windows.dpi(window.id())?;
        let scale = scale_for_dpi(dpi);

        // Only now can the plugin be told its scale: which monitor the window
        // landed on was not knowable before it existed. A plugin that queries the
        // OS itself is allowed to refuse this, so a failure is logged, not fatal.
        if (scale - 1.0).abs() > f64::EPSILON {
            if let Err(err) = gui.set_scale(&mut plugin.plugin_handle(), scale) {
                log::debug!(
                    "CLAP plugin '{}' declined set_scale({scale}): {err}",
                    plugin.info().id
                );
            }
        }

        // The scale may have changed the editor's idea of its own size.
        let size = gui
            .get_size(&mut plugin.plugin_handle())
            .unwrap_or(initial_size);
        if size != initial_size {
            windows.set_client_size(window.id(), to_window_size(size))?;
        }

        // SAFETY: the window is owned by `windows` and is not closed until this
        // session is closed, which destroys the plugin's GUI first. That is
        // exactly the lifetime `set_parent` requires of the handle.
        unsafe {
            gui.set_parent(
                &mut plugin.plugin_handle(),
                Window::from_win32_hwnd(window.hwnd() as *mut c_void),
            )
        }
        .map_err(|err| ClapGuiError::gui("being embedded in the host window", err))?;

        gui.show(&mut plugin.plugin_handle())
            .map_err(|err| ClapGuiError::gui("showing its editor", err))?;
        windows.show_window(window.id())?;

        // Did the plugin actually embed anything?
        //
        // This is not paranoia: `clack-extensions` 0.1.1 discards the plugin's
        // return value from `set_parent` (and `set_size`) in its FFI bridge, so a
        // plugin that *failed* to embed reports success to us. Without this check
        // the user gets an empty window and no explanation. A warning rather than
        // an error: the plugin is otherwise healthy, and a platform that cannot
        // answer the question (`Unsupported`) must not turn "cannot tell" into
        // "broken".
        match windows.has_child_window(window.id()) {
            Ok(false) => log::warn!(
                "CLAP plugin '{}' reported a successful set_parent but embedded no window; \
                 its editor will be blank",
                plugin.info().id
            ),
            Ok(true) | Err(PluginWindowError::Unsupported) => {}
            Err(err) => log::debug!("could not check the plugin window for children: {err}"),
        }

        Ok((size, scale))
    }

    /// Service the editor: apply what the plugin asked for, and answer what the
    /// user did to the window.
    ///
    /// Call it once per main-loop pass, next to
    /// [`LoadedClapPlugin::tick_main_thread`]. Cheap when nothing happened.
    pub fn pump(
        &mut self,
        plugin: &mut LoadedClapPlugin,
        windows: &PluginWindowService,
    ) -> ClapGuiPump {
        catch_unwind(AssertUnwindSafe(|| self.pump_inner(plugin, windows))).unwrap_or_else(|_| {
            log::error!(
                "CLAP plugin '{}' panicked while its editor was being serviced; closing it",
                plugin.info().id
            );
            ClapGuiPump {
                close_requested: true,
                resized_to: None,
            }
        })
    }

    fn pump_inner(
        &mut self,
        plugin: &mut LoadedClapPlugin,
        windows: &PluginWindowService,
    ) -> ClapGuiPump {
        let mut outcome = ClapGuiPump::default();
        let requests = plugin.gui_requests().clone();

        // --- what the plugin asked for -------------------------------------
        if requests.take_hints_changed() {
            // Re-read them so a later `adjust_size` is judged against the current
            // rules; the hints themselves only matter to the frame we draw.
            let _ = self.gui.get_resize_hints(&mut plugin.plugin_handle());
        }
        if let Some(wanted) = requests.take_resize() {
            self.apply_plugin_resize(plugin, windows, wanted, &mut outcome);
        }
        if requests.take_show() {
            if let Err(err) = windows.show_window(self.window.id()) {
                log::warn!("could not show the plugin window: {err}");
            }
        }
        if requests.take_hide() {
            // Hiding is the plugin's business; the window follows. There is no
            // hide in `PluginWindowService` yet, so the honest answer is to close
            // the session -- which is also what the user expects from an editor
            // that asks to disappear.
            outcome.close_requested = true;
        }
        if requests.take_closed() {
            outcome.close_requested = true;
        }

        // --- what the user did ---------------------------------------------
        for event in self.window.drain_events() {
            match event {
                PluginWindowEvent::Closed => outcome.close_requested = true,
                PluginWindowEvent::Resized(size) => {
                    self.apply_window_resize(plugin, windows, to_gui_size(size), &mut outcome);
                }
                PluginWindowEvent::DpiChanged { dpi } => {
                    self.apply_dpi_change(plugin, dpi);
                }
            }
        }

        outcome
    }

    /// The plugin asked for a size (`clap_host_gui->request_resize`).
    fn apply_plugin_resize(
        &mut self,
        plugin: &mut LoadedClapPlugin,
        windows: &PluginWindowService,
        wanted: GuiSize,
        outcome: &mut ClapGuiPump,
    ) {
        if wanted == self.size {
            return;
        }
        // Record before resizing: the `WM_SIZE` this causes arrives in the same
        // pump pass and must be recognised as our own doing.
        self.size = wanted;
        match windows.set_client_size(self.window.id(), to_window_size(wanted)) {
            Ok(()) => outcome.resized_to = Some(wanted),
            Err(err) => {
                log::warn!("could not honour the plugin's resize request: {err}");
                // Tell the plugin what it actually has, as CLAP requires when a
                // request cannot be satisfied.
                self.size = self.current_window_size(windows).unwrap_or(self.size);
                let _ = self.gui.set_size(&mut plugin.plugin_handle(), self.size);
            }
        }
    }

    /// The window changed size (a user drag, usually).
    fn apply_window_resize(
        &mut self,
        plugin: &mut LoadedClapPlugin,
        windows: &PluginWindowService,
        requested: GuiSize,
        outcome: &mut ClapGuiPump,
    ) {
        let adjusted = if self.can_resize {
            self.gui.adjust_size(&mut plugin.plugin_handle(), requested)
        } else {
            None
        };

        match decide_resize(self.size, requested, self.can_resize, adjusted) {
            ResizeDecision::Ignore => {}
            ResizeDecision::RestoreWindow(size) => {
                if let Err(err) = windows.set_client_size(self.window.id(), to_window_size(size)) {
                    log::warn!("could not restore the plugin window size: {err}");
                }
            }
            ResizeDecision::Resize {
                size,
                correct_window,
            } => {
                match self.gui.set_size(&mut plugin.plugin_handle(), size) {
                    Ok(()) => {
                        self.size = size;
                        outcome.resized_to = Some(size);
                        if correct_window {
                            let _ = windows.set_client_size(self.window.id(), to_window_size(size));
                        }
                    }
                    Err(err) => {
                        // The plugin refused after agreeing through `adjust_size`.
                        // Its editor is still the old size, so the window has to
                        // go back or the two disagree on screen.
                        log::debug!(
                            "CLAP plugin '{}' refused set_size({}x{}): {err}",
                            plugin.info().id,
                            size.width,
                            size.height
                        );
                        let _ =
                            windows.set_client_size(self.window.id(), to_window_size(self.size));
                    }
                }
            }
        }
    }

    /// The window moved to a monitor with a different DPI.
    fn apply_dpi_change(&mut self, plugin: &mut LoadedClapPlugin, dpi: u32) {
        let scale = scale_for_dpi(dpi);
        if (scale - self.scale).abs() < f64::EPSILON {
            return;
        }
        self.scale = scale;
        if let Err(err) = self.gui.set_scale(&mut plugin.plugin_handle(), scale) {
            log::debug!(
                "CLAP plugin '{}' declined set_scale({scale}) after a DPI change: {err}",
                plugin.info().id
            );
        }
    }

    fn current_window_size(&self, windows: &PluginWindowService) -> Option<GuiSize> {
        windows.client_size(self.window.id()).ok().map(to_gui_size)
    }

    /// Hide and destroy the editor, then close its window -- in that order,
    /// because the plugin's child window must be gone before its parent is.
    pub fn close(mut self, plugin: &mut LoadedClapPlugin, windows: &PluginWindowService) {
        self.teardown(plugin, windows);
    }

    fn teardown(&mut self, plugin: &mut LoadedClapPlugin, windows: &PluginWindowService) {
        if !self.destroyed {
            self.destroyed = true;
            let self_destroyed = plugin.gui_requests().was_self_destroyed();
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                let _ = self.gui.hide(&mut plugin.plugin_handle());
                // A plugin that reported `closed(was_destroyed = true)` has
                // already freed its GUI; calling destroy again is a double free.
                if !self_destroyed {
                    self.gui.destroy(&mut plugin.plugin_handle());
                }
            }));
            if outcome.is_err() {
                log::error!(
                    "CLAP plugin '{}' panicked while its editor was destroyed; \
                     its window is closed anyway",
                    plugin.info().id
                );
            }
            plugin.gui_requests().clear();
        }
        if let Err(err) = windows.close_window(self.window.id()) {
            // Already closed (a `close_all` at shutdown) is the only realistic
            // way here, and it is not a problem.
            log::debug!("plugin window already closed: {err}");
        }
    }

    /// The size the plugin's editor currently has.
    pub const fn size(&self) -> GuiSize {
        self.size
    }

    /// The GUI scale the plugin was last told about (1.0 = 96 dpi).
    pub const fn scale(&self) -> f64 {
        self.scale
    }

    /// Whether the plugin allows its editor to be resized.
    pub const fn can_resize(&self) -> bool {
        self.can_resize
    }

    /// The host window's id.
    pub const fn window_id(&self) -> PluginWindowId {
        self.window.id()
    }

    /// The host window's `HWND`, as an integer.
    pub const fn hwnd(&self) -> isize {
        self.window.hwnd()
    }

    /// Wake-up ticks the host window has delivered. Non-zero means the
    /// application's message loop is pumping this window.
    pub fn pump_ticks(&self) -> u64 {
        self.window.pump_ticks()
    }
}

impl std::fmt::Debug for ClapGuiSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapGuiSession")
            .field("size", &(self.size.width, self.size.height))
            .field("can_resize", &self.can_resize)
            .field("scale", &self.scale)
            .field("window", &self.window)
            .finish()
    }
}

impl Drop for ClapGuiSession {
    /// A dropped session must not leave a plugin editor embedded in a window that
    /// is about to be destroyed. Without the plugin at hand the GUI cannot be
    /// destroyed here, so this is a loud bug report rather than a cleanup: use
    /// [`close`](Self::close).
    fn drop(&mut self) {
        if !self.destroyed {
            log::error!(
                "a CLAP GUI session was dropped without close(): the plugin still \
                 believes it is embedded in {}",
                self.window.id()
            );
        }
    }
}

fn api_name(api: GuiApiType<'_>) -> &'static str {
    if api == GuiApiType::WIN32 {
        "win32"
    } else if api == GuiApiType::COCOA {
        "cocoa"
    } else if api == GuiApiType::X11 {
        "x11"
    } else if api == GuiApiType::WAYLAND {
        "wayland"
    } else {
        "unknown"
    }
}

fn to_window_size(size: GuiSize) -> WindowSize {
    WindowSize::new(size.width, size.height)
}

fn to_gui_size(size: WindowSize) -> GuiSize {
    GuiSize {
        width: size.width,
        height: size.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: GuiSize = GuiSize {
        width: 400,
        height: 250,
    };

    #[test]
    fn a_resize_to_the_current_size_is_ignored() {
        // This is the echo of the host's own SetWindowPos. Acting on it would
        // make every resize bounce between host and plugin forever.
        assert_eq!(
            decide_resize(CURRENT, CURRENT, true, Some(CURRENT)),
            ResizeDecision::Ignore
        );
    }

    #[test]
    fn a_fixed_size_plugin_gets_its_window_put_back() {
        let dragged = GuiSize {
            width: 800,
            height: 600,
        };
        assert_eq!(
            decide_resize(CURRENT, dragged, false, None),
            ResizeDecision::RestoreWindow(CURRENT)
        );
        // Even if the plugin answered adjust_size, which it should not have.
        assert_eq!(
            decide_resize(CURRENT, dragged, false, Some(dragged)),
            ResizeDecision::RestoreWindow(CURRENT)
        );
    }

    #[test]
    fn an_accepted_size_needs_no_window_correction() {
        let dragged = GuiSize {
            width: 600,
            height: 400,
        };
        assert_eq!(
            decide_resize(CURRENT, dragged, true, Some(dragged)),
            ResizeDecision::Resize {
                size: dragged,
                correct_window: false
            }
        );
    }

    #[test]
    fn an_adjusted_size_is_applied_and_the_window_follows() {
        let dragged = GuiSize {
            width: 617,
            height: 433,
        };
        let rounded = GuiSize {
            width: 610,
            height: 430,
        };
        assert_eq!(
            decide_resize(CURRENT, dragged, true, Some(rounded)),
            ResizeDecision::Resize {
                size: rounded,
                correct_window: true
            }
        );
    }

    #[test]
    fn a_plugin_without_adjust_size_gets_the_request_verbatim() {
        let dragged = GuiSize {
            width: 617,
            height: 433,
        };
        assert_eq!(
            decide_resize(CURRENT, dragged, true, None),
            ResizeDecision::Resize {
                size: dragged,
                correct_window: false
            }
        );
    }

    #[test]
    fn a_drag_the_plugin_rounds_back_to_its_current_size_only_moves_the_window() {
        // Dragging 3 px on an editor with a 10 px quantum: the plugin has nothing
        // to do, but the window is 3 px wrong and must be corrected.
        let dragged = GuiSize {
            width: 403,
            height: 250,
        };
        assert_eq!(
            decide_resize(CURRENT, dragged, true, Some(CURRENT)),
            ResizeDecision::RestoreWindow(CURRENT)
        );
    }

    #[test]
    fn dpi_maps_to_the_scale_windows_plugins_expect() {
        assert_eq!(scale_for_dpi(96), 1.0);
        assert_eq!(scale_for_dpi(144), 1.5);
        assert_eq!(scale_for_dpi(192), 2.0);
        // A window that reports nothing is not a reason to hand a plugin a 0.0
        // scale and watch it divide by it.
        assert_eq!(scale_for_dpi(0), 1.0);
    }

    #[test]
    fn sizes_survive_the_round_trip_between_the_two_crates() {
        let size = GuiSize {
            width: 1234,
            height: 567,
        };
        assert_eq!(to_gui_size(to_window_size(size)), size);
    }

    #[test]
    fn the_api_name_is_reported_for_the_error_message() {
        assert_eq!(api_name(GuiApiType::WIN32), "win32");
        assert_eq!(api_name(GuiApiType::COCOA), "cocoa");
        assert_eq!(api_name(GuiApiType(c"something-else")), "unknown");
    }

    #[test]
    fn the_pump_outcome_defaults_to_nothing_happened() {
        let pump = ClapGuiPump::default();
        assert!(!pump.close_requested);
        assert!(pump.resized_to.is_none());
    }
}
