//! A VST3 plugin's editor in a native WAVE window. UI thread only.
//!
//! ```text
//!  createView("editor")                does this plugin draw at all?
//!  isPlatformTypeSupported(HWND)       will it embed in a Win32 window?
//!  getSize()                           the size to open the window at
//!  --- open the window, hidden ---
//!  setContentScaleFactor(dpi / 96)     only now is the monitor's DPI known
//!  setFrame(host frame)                BEFORE attached, or the editor cannot resize itself
//!  attached(HWND, "HWND")              the plugin creates its child window
//!  getSize()                           may have changed with the scale
//!  --- size the window, show it ---
//! ```
//!
//! Resizing is a negotiation. VST3 has `checkSizeConstraint`, which the plugin
//! **edits in place**: the host writes the requested rect, calls, and reads the
//! accepted size back. Plugin-initiated resizes (`IPlugFrame::resizeView`) are
//! latched and applied at the next window tick.

use std::sync::Arc;

use vst3::Steinberg::Vst::{IEditController, IEditControllerTrait, ViewType};
use vst3::Steinberg::{
    kResultOk, kResultTrue, FIDString, IPlugFrame, IPlugView, IPlugViewContentScaleSupport,
    IPlugViewContentScaleSupportTrait, IPlugViewTrait, ViewRect,
};
use vst3::{ComPtr, ComWrapper};
use wave_plugin_windows::{Service, Window, WindowEvent, WindowOptions, WindowSize};

use crate::host::{GuiRequests, PlugFrame};
use crate::Vst3Error;

/// Size used when a plugin claims an editor but will not say how big it is.
const FALLBACK_SIZE: WindowSize = WindowSize {
    width: 400,
    height: 300,
};

/// 96 dpi is 100% on Windows.
const REFERENCE_DPI: u32 = 96;

#[cfg(windows)]
const PLATFORM_TYPE: (FIDString, &str) = (vst3::Steinberg::kPlatformTypeHWND, "HWND");
#[cfg(not(windows))]
const PLATFORM_TYPE: (FIDString, &str) = (
    vst3::Steinberg::kPlatformTypeX11EmbedWindowID,
    "X11EmbedWindowID",
);

pub(crate) fn scale_for_dpi(dpi: u32) -> f32 {
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / REFERENCE_DPI as f32
    }
}

pub(crate) fn rect_to_size(rect: &ViewRect) -> WindowSize {
    WindowSize::new(
        u32::try_from(rect.right - rect.left).unwrap_or(0),
        u32::try_from(rect.bottom - rect.top).unwrap_or(0),
    )
}

pub(crate) fn size_to_rect(size: WindowSize) -> ViewRect {
    ViewRect {
        left: 0,
        top: 0,
        right: i32::try_from(size.width).unwrap_or(i32::MAX),
        bottom: i32::try_from(size.height).unwrap_or(i32::MAX),
    }
}

/// An open editor: the plugin's view plus the window it lives in.
///
/// Field order is the teardown order: the view (which holds a pointer to the
/// frame) goes before the frame, and both before the window.
pub(crate) struct EditorSession {
    view: Option<ComPtr<IPlugView>>,
    frame: ComWrapper<PlugFrame>,
    window: Window,
    requests: Arc<GuiRequests>,
    size: WindowSize,
    can_resize: bool,
    scale: f32,
}

impl EditorSession {
    /// Create the view and embed it in a new window. UI thread.
    pub(crate) fn open(
        controller: &ComPtr<IEditController>,
        windows: &Service,
        title: &str,
        requests: Arc<GuiRequests>,
    ) -> Result<Self, Vst3Error> {
        if !windows.on_owner_thread() {
            return Err(Vst3Error::WrongThread);
        }
        // SAFETY: the controller is live and initialised; `createView` returns
        // an addRef'ed view or null, which is `from_raw`'s contract.
        let view = unsafe { ComPtr::<IPlugView>::from_raw(controller.createView(ViewType::kEditor)) }
            .ok_or(Vst3Error::NoEditor)?;
        // SAFETY: live view, static C string.
        if unsafe { view.isPlatformTypeSupported(PLATFORM_TYPE.0) } != kResultTrue {
            return Err(Vst3Error::UnsupportedEditorApi { api: PLATFORM_TYPE.1 });
        }
        requests.clear();
        // SAFETY: live view, live out-parameter.
        let (initial, can_resize) = unsafe {
            let mut rect: ViewRect = std::mem::zeroed();
            let size = if view.getSize(&mut rect) == kResultOk {
                rect_to_size(&rect)
            } else {
                FALLBACK_SIZE
            };
            let size = if size.width == 0 || size.height == 0 {
                FALLBACK_SIZE
            } else {
                size
            };
            (size, view.canResize() == kResultTrue)
        };
        let window = windows.open_window(
            &WindowOptions::new(format!("{title} - WAVE"), initial).resizable(can_resize),
        )?;
        let mut session = Self {
            view: Some(view),
            frame: PlugFrame::new(Arc::clone(&requests)),
            window,
            requests,
            size: initial,
            can_resize,
            scale: 1.0,
        };
        if let Err(err) = session.attach() {
            session.close();
            return Err(err);
        }
        Ok(session)
    }

    fn attach(&mut self) -> Result<(), Vst3Error> {
        let view = self.view.as_ref().expect("live until close");
        self.scale = scale_for_dpi(self.window.dpi().unwrap_or(REFERENCE_DPI));
        // SAFETY: live view; every pointer is to a live local or to the frame
        // this session owns (dropped after the view). The VST3 attach order.
        unsafe {
            if (self.scale - 1.0).abs() > f32::EPSILON {
                if let Some(scaling) = view.cast::<IPlugViewContentScaleSupport>() {
                    // A plugin that reads the OS scale itself may refuse.
                    let _ = scaling.setContentScaleFactor(self.scale);
                }
            }
            if let Some(frame) = self.frame.as_com_ref::<IPlugFrame>() {
                let _ = view.setFrame(frame.as_ptr());
            }
            let result = view.attached(
                self.window.native_handle() as *mut std::ffi::c_void,
                PLATFORM_TYPE.0,
            );
            if result != kResultOk {
                return Err(Vst3Error::Editor {
                    stage: "being embedded in its window",
                    result,
                });
            }
            let mut rect: ViewRect = std::mem::zeroed();
            if view.getSize(&mut rect) == kResultOk {
                let size = rect_to_size(&rect);
                if size.width > 0 && size.height > 0 {
                    self.size = size;
                }
            }
        }
        self.window.set_client_size(self.size.clamped())?;
        self.window.show()?;
        if let Ok(false) = self.window.has_child_window() {
            log::warn!("'{}': the VST3 editor reported a successful attach but embedded no window", self.window.id());
        }
        Ok(())
    }

    /// Bring the window to the front (the editor is already open).
    pub(crate) fn raise(&self) {
        let _ = self.window.show();
    }

    pub(crate) fn window(&self) -> &Window {
        &self.window
    }

    /// Service the editor at a window tick. Returns `true` when the user closed
    /// the window, in which case the caller calls [`close`](Self::close).
    pub(crate) fn pump(&mut self) -> bool {
        if let Some((width, height)) = self.requests.take_resize() {
            let wanted = WindowSize::new(u32::try_from(width).unwrap_or(0), u32::try_from(height).unwrap_or(0));
            if wanted.width > 0 && wanted.height > 0 && wanted != self.size {
                // Record first: the resize event this causes is our own echo.
                self.size = wanted;
                match self.window.set_client_size(wanted.clamped()) {
                    Ok(()) => self.tell_view_its_size(wanted),
                    Err(err) => log::warn!("could not honour the editor's resize request: {err}"),
                }
            }
        }
        let mut closed = false;
        for event in self.window.drain_events() {
            match event {
                WindowEvent::Closed => closed = true,
                WindowEvent::Resized(size) => self.apply_window_resize(size),
                WindowEvent::DpiChanged { dpi } => self.apply_dpi_change(dpi),
            }
        }
        closed
    }

    fn apply_window_resize(&mut self, requested: WindowSize) {
        if requested == self.size || requested.width == 0 || requested.height == 0 {
            // Our own SetWindowPos echoing back, or a minimise.
            return;
        }
        if !self.can_resize {
            let _ = self.window.set_client_size(self.size.clamped());
            return;
        }
        let agreed = self.constrain(requested);
        if agreed == self.size {
            let _ = self.window.set_client_size(self.size.clamped());
            return;
        }
        self.size = agreed;
        self.tell_view_its_size(agreed);
        if agreed != requested {
            let _ = self.window.set_client_size(agreed.clamped());
        }
    }

    /// What the plugin accepts nearest to `requested`.
    fn constrain(&self, requested: WindowSize) -> WindowSize {
        let Some(view) = self.view.as_ref() else {
            return requested;
        };
        let mut rect = size_to_rect(requested);
        // SAFETY: live view, live in/out rect.
        if unsafe { view.checkSizeConstraint(&mut rect) } != kResultTrue {
            return requested;
        }
        let constrained = rect_to_size(&rect);
        if constrained.width == 0 || constrained.height == 0 {
            requested
        } else {
            constrained
        }
    }

    fn tell_view_its_size(&self, size: WindowSize) {
        if let Some(view) = self.view.as_ref() {
            let mut rect = size_to_rect(size);
            // SAFETY: live view, live rect.
            let _ = unsafe { view.onSize(&mut rect) };
        }
    }

    fn apply_dpi_change(&mut self, dpi: u32) {
        let scale = scale_for_dpi(dpi);
        if (scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        self.scale = scale;
        if let Some(scaling) = self.view.as_ref().and_then(|view| view.cast::<IPlugViewContentScaleSupport>()) {
            // SAFETY: live interface, scalar argument.
            let _ = unsafe { scaling.setContentScaleFactor(scale) };
        }
    }

    /// Detach the editor, then destroy its window -- in that order: the
    /// plugin's child window must go before its parent. Idempotent.
    pub(crate) fn close(&mut self) {
        if let Some(view) = self.view.take() {
            // SAFETY: the view is live; `removed` destroys the plugin's child
            // window, and clearing the frame leaves it no pointer to an object
            // about to be dropped.
            unsafe {
                let _ = view.removed();
                let _ = view.setFrame(std::ptr::null_mut());
            }
            drop(view);
        }
        self.requests.clear();
        let _ = self.window.set_tick_handler(None);
        if let Err(err) = self.window.close() {
            log::error!("could not close a VST3 editor window: {err}");
        }
    }
}

impl Drop for EditorSession {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_rects_and_window_sizes_convert_without_wrapping() {
        let rect = ViewRect { left: 10, top: 20, right: 810, bottom: 620 };
        assert_eq!(rect_to_size(&rect), WindowSize::new(800, 600));
        let at_origin = size_to_rect(WindowSize::new(640, 480));
        assert_eq!((at_origin.left, at_origin.top, at_origin.right, at_origin.bottom), (0, 0, 640, 480));
        let inverted = ViewRect { left: 800, top: 600, right: 0, bottom: 0 };
        assert_eq!(rect_to_size(&inverted), WindowSize::new(0, 0), "no gigantic window from an underflow");
    }

    #[test]
    fn dpi_maps_to_the_scale_vst3_editors_expect() {
        assert!((scale_for_dpi(96) - 1.0).abs() < f32::EPSILON);
        assert!((scale_for_dpi(144) - 1.5).abs() < f32::EPSILON);
        assert!((scale_for_dpi(0) - 1.0).abs() < f32::EPSILON, "never a 0.0 scale");
        assert!(FALLBACK_SIZE.is_within_limits());
    }
}
