//! Showing a VST3 plugin's editor in a native MYDAW window.
//!
//! [`hosting`](crate::hosting) makes a plugin *process audio*; this module makes
//! it *visible*. It is the VST3 counterpart of `mydaw-plugin-host-clap`'s `gui`
//! module, and the interesting differences are all VST3's:
//!
//! - **The editor belongs to the controller, not the plugin.** A plugin whose
//!   controller would not initialise has no editor, and that is a normal state
//!   rather than an error.
//! - **`IPlugFrame` must be set before `attached`.** A plugin that cannot find
//!   its frame during attach either refuses or silently gives up its ability to
//!   resize itself, depending on the vendor.
//! - **There is no `adjust_size`; there is `checkSizeConstraint`,** which the
//!   plugin *edits in place*. So the negotiation reads back the rect rather than
//!   receiving a new one.
//!
//! # The sequence, and why it is in this order
//!
//! ```text
//!  createView("editor")                does this plugin draw at all?
//!  isPlatformTypeSupported(HWND)       will it embed in a Win32 window?
//!  getSize()                           the size to make the window
//!  --- host opens the window, hidden ---
//!  setContentScaleFactor(dpi / 96)     only now is the monitor's DPI known
//!  setFrame(host frame)                before attach, see above
//!  attached(HWND, HWND type)           plugin creates its child window
//!  getSize()                           may have changed with the scale
//!  --- host sizes and shows the window ---
//! ```
//!
//! The window is opened *hidden* and shown last, so the user never sees an empty
//! frame waiting for a plugin to draw into it.
//!
//! # Threading
//! Everything here is main-thread work. It takes `&mut LoadedVst3Plugin` (which
//! is `!Send`) and a `&PluginWindowService` (which refuses calls from other
//! threads), so there is no way to get it wrong by accident.

use std::panic::{catch_unwind, AssertUnwindSafe};

use mydaw_plugin_windows::{
    PluginWindowError, PluginWindowEvent, PluginWindowHandle, PluginWindowId, PluginWindowService,
    WindowOptions, WindowSize,
};
use vst3::ComPtr;
use vst3::Steinberg::Vst::{IEditControllerTrait, ViewType};
use vst3::Steinberg::{
    kPlatformTypeHWND, IPlugFrame, IPlugView, IPlugViewContentScaleSupport,
    IPlugViewContentScaleSupportTrait, IPlugViewTrait, ViewRect,
};
use vst3::Steinberg::{kResultOk, kResultTrue};

use crate::host::PlugFrame;
use crate::hosting::LoadedVst3Plugin;

/// Size used when a plugin claims an editor but will not say how big it is.
pub const FALLBACK_SIZE: WindowSize = WindowSize {
    width: 400,
    height: 300,
};

/// Reference DPI: 96 is 100% scaling on Windows.
pub const REFERENCE_DPI: u32 = 96;

/// The content scale factor a plugin should be told about at this DPI.
pub fn scale_for_dpi(dpi: u32) -> f32 {
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / REFERENCE_DPI as f32
    }
}

/// Everything that can go wrong between "the user clicked Open GUI" and "the
/// editor is on screen".
#[derive(Debug, thiserror::Error)]
pub enum Vst3GuiError {
    /// The plugin has no edit controller, or its controller returned no view.
    /// Common and not a malfunction: plenty of effects have no editor.
    #[error("this plugin has no editor")]
    NoEditor,

    /// The plugin has an editor but will not embed in a Win32 window.
    #[error("this plugin's editor does not support the {api} windowing API")]
    UnsupportedApi { api: &'static str },

    /// `IPlugView::attached` refused.
    #[error("the plugin refused while {stage} (result {result:#x})")]
    View { stage: &'static str, result: i32 },

    /// The native window could not be created or driven.
    #[error("the plugin window could not be opened: {0}")]
    Window(#[from] PluginWindowError),

    /// Plugin code panicked through the FFI boundary while its editor was being
    /// set up. The session is abandoned; the plugin keeps processing audio.
    #[error("the plugin panicked while {stage}")]
    Panicked { stage: &'static str },
}

/// What one [`Vst3GuiSession::pump`] pass observed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Vst3GuiPump {
    /// The user closed the window. The owner should call
    /// [`Vst3GuiSession::close`].
    pub close_requested: bool,
    /// The size the editor ended up at, if it changed during this pass.
    pub resized_to: Option<WindowSize>,
}

/// Turn a `ViewRect` into the size of the window that has to hold it.
pub fn rect_to_size(rect: &ViewRect) -> WindowSize {
    WindowSize::new(
        u32::try_from(rect.right - rect.left).unwrap_or(0),
        u32::try_from(rect.bottom - rect.top).unwrap_or(0),
    )
}

/// A rect at the origin with this size, which is what `onSize` wants.
pub fn size_to_rect(size: WindowSize) -> ViewRect {
    ViewRect {
        left: 0,
        top: 0,
        right: i32::try_from(size.width).unwrap_or(i32::MAX),
        bottom: i32::try_from(size.height).unwrap_or(i32::MAX),
    }
}

/// An open plugin editor: the plugin's `IPlugView` plus the window it lives in.
///
/// Not `Send`: it holds a [`PluginWindowHandle`], and every call it makes is
/// main-thread work.
///
/// # Field order is the teardown protocol
/// The view is declared before the frame it was given, because a view still
/// holding an `IPlugFrame` pointer whose object has been freed is a
/// use-after-free on the plugin's next resize.
pub struct Vst3GuiSession {
    view: Option<ComPtr<IPlugView>>,
    /// The `IPlugFrame` the view was given. Must outlive the view.
    frame: vst3::ComWrapper<PlugFrame>,
    window: PluginWindowHandle,
    /// The size the plugin last agreed to.
    size: WindowSize,
    can_resize: bool,
    scale: f32,
    /// Set once the view has been removed, so [`close`](Self::close) and `Drop`
    /// cannot do it twice.
    detached: bool,
}

impl Vst3GuiSession {
    /// Open the plugin's editor in a new native window.
    ///
    /// # Errors
    /// [`Vst3GuiError::NoEditor`] when the plugin has none (the common case, and
    /// not a failure of anything), plus every way the plugin or the window
    /// system can refuse.
    pub fn open(
        plugin: &mut LoadedVst3Plugin,
        windows: &PluginWindowService,
        title: &str,
    ) -> Result<Self, Vst3GuiError> {
        catch_unwind(AssertUnwindSafe(|| {
            Self::open_inner(plugin, windows, title)
        }))
        .map_err(|_| Vst3GuiError::Panicked {
            stage: "opening its editor",
        })?
    }

    fn open_inner(
        plugin: &mut LoadedVst3Plugin,
        windows: &PluginWindowService,
        title: &str,
    ) -> Result<Self, Vst3GuiError> {
        let controller = plugin.controller().ok_or(Vst3GuiError::NoEditor)?;

        // SAFETY: the controller is live and initialised. `createView` returns
        // an already-`addRef`ed view or null, which is exactly `ComPtr::from_raw`.
        let view = unsafe {
            let raw = controller.createView(ViewType::kEditor);
            ComPtr::<IPlugView>::from_raw(raw)
        }
        .ok_or(Vst3GuiError::NoEditor)?;

        // SAFETY: `view` is live; the platform string is a `'static` C literal.
        if unsafe { view.isPlatformTypeSupported(kPlatformTypeHWND) } != kResultTrue {
            return Err(Vst3GuiError::UnsupportedApi { api: "HWND" });
        }

        // Stale flags from a previous session must not be applied to this one.
        plugin.gui_requests().clear();
        let frame = PlugFrame::new(std::sync::Arc::clone(plugin.gui_requests()));

        // SAFETY: `view` is live and `rect` is a live out-parameter. A plugin
        // that refuses to state its size gets the fallback rather than a
        // zero-pixel window.
        let (initial, can_resize) = unsafe {
            let mut rect: ViewRect = std::mem::zeroed();
            let size = if view.getSize(&mut rect) == kResultOk {
                let size = rect_to_size(&rect);
                if size.width == 0 || size.height == 0 {
                    FALLBACK_SIZE
                } else {
                    size
                }
            } else {
                FALLBACK_SIZE
            };
            (size, view.canResize() == kResultTrue)
        };

        let options = WindowOptions::new(title, initial.clamped()).resizable(can_resize);
        let window = windows.open_window(&options)?;

        let mut session = Self {
            view: Some(view),
            frame,
            window,
            size: initial,
            can_resize,
            scale: 1.0,
            detached: false,
        };

        match session.attach(windows) {
            Ok(()) => Ok(session),
            Err(err) => {
                // The window exists but the plugin never got into it. `close`
                // handles both halves in the right order.
                session.teardown(plugin, windows);
                Err(err)
            }
        }
    }

    /// The half of the sequence that needs a live window: scale, frame, attach,
    /// size, show.
    fn attach(&mut self, windows: &PluginWindowService) -> Result<(), Vst3GuiError> {
        let view = self.view.as_ref().expect("live until teardown");
        let dpi = windows.dpi(self.window.id()).unwrap_or(REFERENCE_DPI);
        self.scale = scale_for_dpi(dpi);

        // SAFETY: `view` is live; every pointer below is to a live local or to
        // the frame this session owns (declared after the view, so it outlives
        // it). This is the attach order from the VST3 specification.
        unsafe {
            if (self.scale - 1.0).abs() > f32::EPSILON {
                if let Some(scaling) = view.cast::<IPlugViewContentScaleSupport>() {
                    // A plugin that scales itself from the OS is entitled to
                    // refuse; that is a debug note, not a failure.
                    let result = scaling.setContentScaleFactor(self.scale);
                    if result != kResultOk {
                        log::debug!("VST3 editor declined setContentScaleFactor({})", self.scale);
                    }
                }
            }

            if let Some(frame) = self.frame.as_com_ref::<IPlugFrame>() {
                let result = view.setFrame(frame.as_ptr());
                if result != kResultOk {
                    // Not fatal: a plugin that cannot hold a frame simply cannot
                    // ask to be resized.
                    log::debug!("VST3 editor declined setFrame ({result:#x})");
                }
            }

            let result = view.attached(
                self.window.hwnd() as *mut std::ffi::c_void,
                kPlatformTypeHWND,
            );
            if result != kResultOk {
                return Err(Vst3GuiError::View {
                    stage: "being embedded in the host window",
                    result,
                });
            }

            // The scale and the attach may both have changed the editor's idea
            // of its own size.
            let mut rect: ViewRect = std::mem::zeroed();
            if view.getSize(&mut rect) == kResultOk {
                let size = rect_to_size(&rect);
                if size.width > 0 && size.height > 0 {
                    self.size = size;
                }
            }
        }

        windows.set_client_size(self.window.id(), self.size.clamped())?;
        windows.show_window(self.window.id())?;

        // Did the plugin actually embed anything? A warning rather than an
        // error: the plugin is otherwise healthy, and a platform that cannot
        // answer the question must not turn "cannot tell" into "broken".
        match windows.has_child_window(self.window.id()) {
            Ok(false) => log::warn!(
                "a VST3 editor reported a successful attach but embedded no window; \
                 it will be blank"
            ),
            Ok(true) | Err(PluginWindowError::Unsupported) => {}
            Err(err) => log::debug!("could not check the plugin window for children: {err}"),
        }
        Ok(())
    }

    /// Service the editor: apply what the plugin asked for, and answer what the
    /// user did to the window.
    ///
    /// Call it once per main-loop pass. Cheap when nothing happened.
    pub fn pump(
        &mut self,
        plugin: &mut LoadedVst3Plugin,
        windows: &PluginWindowService,
    ) -> Vst3GuiPump {
        catch_unwind(AssertUnwindSafe(|| self.pump_inner(plugin, windows))).unwrap_or_else(|_| {
            log::error!(
                "VST3 plugin '{}' panicked while its editor was being serviced; closing it",
                plugin.info().name
            );
            Vst3GuiPump {
                close_requested: true,
                resized_to: None,
            }
        })
    }

    fn pump_inner(
        &mut self,
        plugin: &mut LoadedVst3Plugin,
        windows: &PluginWindowService,
    ) -> Vst3GuiPump {
        let mut outcome = Vst3GuiPump::default();

        // --- what the plugin asked for -------------------------------------
        if let Some((width, height)) = plugin.gui_requests().take_resize() {
            let wanted = WindowSize::new(
                u32::try_from(width).unwrap_or(0),
                u32::try_from(height).unwrap_or(0),
            );
            if wanted.width > 0 && wanted.height > 0 && wanted != self.size {
                // Record before resizing: the resize event this causes arrives
                // in the same pump pass and must be recognised as our own doing.
                self.size = wanted;
                match windows.set_client_size(self.window.id(), wanted.clamped()) {
                    Ok(()) => {
                        self.tell_view_its_size(wanted);
                        outcome.resized_to = Some(wanted);
                    }
                    Err(err) => log::warn!("could not honour the editor's resize request: {err}"),
                }
            }
        }

        // --- what the user did ---------------------------------------------
        for event in self.window.drain_events() {
            match event {
                PluginWindowEvent::Closed => outcome.close_requested = true,
                PluginWindowEvent::Resized(size) => {
                    self.apply_window_resize(windows, size, &mut outcome);
                }
                PluginWindowEvent::DpiChanged { dpi } => self.apply_dpi_change(dpi),
            }
        }

        outcome
    }

    /// The window changed size (a user drag, usually).
    ///
    /// VST3's negotiation is `checkSizeConstraint`, which the plugin **edits in
    /// place**: it is handed the requested rect and writes back the nearest size
    /// it will actually accept. Skipping the read-back is the classic host bug
    /// that leaves editors clipped.
    fn apply_window_resize(
        &mut self,
        windows: &PluginWindowService,
        requested: WindowSize,
        outcome: &mut Vst3GuiPump,
    ) {
        if requested == self.size {
            // The echo of our own `SetWindowPos`. Acting on it would start a
            // resize loop between host and plugin.
            return;
        }
        if !self.can_resize {
            if let Err(err) = windows.set_client_size(self.window.id(), self.size.clamped()) {
                log::warn!("could not restore a fixed-size editor's window: {err}");
            }
            return;
        }

        let agreed = self.constrain(requested);
        if agreed == self.size {
            // The plugin rounded the drag back to the size it already has: it
            // needs no call, but the window is now wrong.
            let _ = windows.set_client_size(self.window.id(), self.size.clamped());
            return;
        }

        self.size = agreed;
        self.tell_view_its_size(agreed);
        if agreed != requested {
            let _ = windows.set_client_size(self.window.id(), agreed.clamped());
        }
        outcome.resized_to = Some(agreed);
    }

    /// Ask the plugin what it will accept nearest to `requested`.
    fn constrain(&self, requested: WindowSize) -> WindowSize {
        let Some(view) = self.view.as_ref() else {
            return requested;
        };
        let mut rect = size_to_rect(requested);
        // SAFETY: `view` is live and `rect` is a live in/out parameter, which is
        // exactly what `checkSizeConstraint` takes.
        let accepted = unsafe { view.checkSizeConstraint(&mut rect) } == kResultTrue;
        if !accepted {
            return requested;
        }
        let constrained = rect_to_size(&rect);
        if constrained.width == 0 || constrained.height == 0 {
            requested
        } else {
            constrained
        }
    }

    /// Tell the view the size it now has.
    fn tell_view_its_size(&self, size: WindowSize) {
        let Some(view) = self.view.as_ref() else {
            return;
        };
        let mut rect = size_to_rect(size);
        // SAFETY: `view` is live and `rect` is a live out-parameter.
        let result = unsafe { view.onSize(&mut rect) };
        if result != kResultOk {
            log::debug!(
                "VST3 editor declined onSize({}x{})",
                size.width,
                size.height
            );
        }
    }

    /// The window moved to a monitor with a different DPI.
    fn apply_dpi_change(&mut self, dpi: u32) {
        let scale = scale_for_dpi(dpi);
        if (scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        self.scale = scale;
        let Some(view) = self.view.as_ref() else {
            return;
        };
        // SAFETY: `view` is live; the argument is a plain scalar.
        unsafe {
            if let Some(scaling) = view.cast::<IPlugViewContentScaleSupport>() {
                let _ = scaling.setContentScaleFactor(scale);
            }
        }
    }

    /// Detach and release the editor, then close its window -- in that order,
    /// because the plugin's child window must be gone before its parent is.
    pub fn close(mut self, plugin: &mut LoadedVst3Plugin, windows: &PluginWindowService) {
        self.teardown(plugin, windows);
    }

    fn teardown(&mut self, plugin: &mut LoadedVst3Plugin, windows: &PluginWindowService) {
        if !self.detached {
            self.detached = true;
            // SAFETY: the view is live and attached. `removed` destroys the
            // plugin's child window; clearing the frame afterwards makes sure
            // the view holds no pointer to an object about to be dropped.
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                if let Some(view) = self.view.as_ref() {
                    unsafe {
                        let _ = view.removed();
                        let _ = view.setFrame(std::ptr::null_mut());
                    }
                }
            }));
            if outcome.is_err() {
                log::error!(
                    "VST3 plugin '{}' panicked while its editor was destroyed; its window is \
                     closed anyway",
                    plugin.info().name
                );
            }
            // Release the view before the frame it was pointing at.
            drop(self.view.take());
            plugin.gui_requests().clear();
        }
        if let Err(err) = windows.close_window(self.window.id()) {
            // Already closed (a `close_all` at shutdown) is the only realistic
            // way here, and it is not a problem.
            log::debug!("plugin window already closed: {err}");
        }
    }

    /// The size the plugin's editor currently has.
    pub const fn size(&self) -> WindowSize {
        self.size
    }

    /// The content scale the plugin was last told about (1.0 = 96 dpi).
    pub const fn scale(&self) -> f32 {
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

impl std::fmt::Debug for Vst3GuiSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3GuiSession")
            .field("size", &(self.size.width, self.size.height))
            .field("can_resize", &self.can_resize)
            .field("scale", &self.scale)
            .field("window", &self.window)
            .finish()
    }
}

impl Drop for Vst3GuiSession {
    /// A dropped session must not leave an editor embedded in a window that is
    /// about to be destroyed. Without the plugin at hand the view cannot be
    /// detached here, so this is a loud bug report rather than a cleanup: use
    /// [`close`](Self::close).
    fn drop(&mut self) {
        if !self.detached {
            log::error!(
                "a VST3 GUI session was dropped without close(): the plugin still believes it \
                 is embedded in {}",
                self.window.id()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_rect_becomes_the_size_of_the_window_that_holds_it() {
        // The plugin reports corners; the window service wants extents, and a
        // view whose origin is not (0, 0) is legal.
        let rect = ViewRect {
            left: 10,
            top: 20,
            right: 810,
            bottom: 620,
        };
        assert_eq!(rect_to_size(&rect), WindowSize::new(800, 600));

        let at_origin = size_to_rect(WindowSize::new(640, 480));
        assert_eq!((at_origin.left, at_origin.top), (0, 0));
        assert_eq!((at_origin.right, at_origin.bottom), (640, 480));
        assert_eq!(rect_to_size(&at_origin), WindowSize::new(640, 480));
    }

    #[test]
    fn an_inverted_rect_reads_as_zero_rather_than_wrapping() {
        // A plugin that fills the rect backwards must not produce a gigantic
        // window from an underflowed subtraction.
        let inverted = ViewRect {
            left: 800,
            top: 600,
            right: 0,
            bottom: 0,
        };
        assert_eq!(rect_to_size(&inverted), WindowSize::new(0, 0));
    }

    #[test]
    fn dpi_maps_to_the_scale_vst3_editors_expect() {
        assert!((scale_for_dpi(96) - 1.0).abs() < f32::EPSILON);
        assert!((scale_for_dpi(144) - 1.5).abs() < f32::EPSILON);
        assert!((scale_for_dpi(192) - 2.0).abs() < f32::EPSILON);
        // A window that reports nothing is not a reason to hand a plugin a 0.0
        // scale and watch it divide by it.
        assert!((scale_for_dpi(0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_fallback_size_is_a_window_a_user_can_actually_see() {
        const { assert!(FALLBACK_SIZE.width >= 200 && FALLBACK_SIZE.height >= 100) };
        assert!(FALLBACK_SIZE.is_within_limits());
    }

    #[test]
    fn the_pump_outcome_defaults_to_nothing_happened() {
        let pump = Vst3GuiPump::default();
        assert!(!pump.close_requested);
        assert!(pump.resized_to.is_none());
    }

    #[test]
    fn the_no_editor_error_reads_as_plain_language() {
        // A plugin without a GUI is an ordinary plugin, and the status bar shows
        // this sentence as-is.
        assert_eq!(
            Vst3GuiError::NoEditor.to_string(),
            "this plugin has no editor"
        );
    }
}
