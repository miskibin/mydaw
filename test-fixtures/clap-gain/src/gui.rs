//! The fixture's `clap.gui` implementation: a small, deliberately *observable*
//! Win32 editor.
//!
//! Spike 3 hosts plugin GUIs. Testing that against whichever plugin happens to be
//! installed would make the assertions depend on a vendor, a machine and a day --
//! the same argument that made this crate a real CLAP plugin in spike 2. So the
//! fixture grows a real editor, designed so that every host-side promise is
//! *checkable*:
//!
//! | Host promise | How this editor makes it observable |
//! | --- | --- |
//! | The window is parented and sized | The editor is a `WS_CHILD` of the host's `HWND`; it paints its own size |
//! | The host services `clap.timer-support` | The animation and the frame counter advance **only** from `on_timer` |
//! | The host's loop pumps windows it did not create | The child window repaints at all |
//! | `set_size` is really called | The painted size text and `get_size` follow it |
//! | Resize is negotiated, not assumed | [`adjust_size`] rounds down to [`SIZE_QUANTUM`], so a host that skips the negotiation ends up with a size the plugin never agreed to |
//!
//! The frame counter is also published as the child window's title text
//! (`SetWindowTextW`), which is the one piece of GUI state a host-side *test* can
//! read back through Win32 without a screenshot. That is how
//! `plugins/host-clap/tests/fixture_gui.rs` proves liveness automatically rather
//! than by eye.
//!
//! There is no Win32 code outside this file and `plugins/plugin-windows`.

use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, Window};
use clack_extensions::timer::{HostTimer, TimerId};
use clack_plugin::plugin::PluginError;
use clack_plugin::prelude::HostMainThreadHandle;

/// Size the editor asks for when it is first created.
pub const PREFERRED_SIZE: GuiSize = GuiSize {
    width: 400,
    height: 250,
};

/// Smallest size the editor will agree to.
pub const MIN_SIZE: GuiSize = GuiSize {
    width: 200,
    height: 120,
};

/// Largest size the editor will agree to.
pub const MAX_SIZE: GuiSize = GuiSize {
    width: 1600,
    height: 1000,
};

/// The editor only accepts sizes that are multiples of this.
///
/// Real plugins have constraints like this (fixed pixel art, aspect ratios, tile
/// grids). Having one here means a host that resizes without calling
/// `adjust_size` first can be caught doing it.
pub const SIZE_QUANTUM: u32 = 10;

/// Period of the CLAP timer the editor registers with the host, in milliseconds.
/// ~30 Hz: the rate CLAP asks every host to be able to provide.
pub const TIMER_PERIOD_MS: u32 = 33;

/// Round `size` down to something this editor will accept.
///
/// CLAP requires `adjust_size` to return dimensions **smaller than or equal to**
/// the requested ones, so this only ever rounds down -- except against the
/// minimum, which wins over the request because an editor smaller than its
/// controls is useless.
pub fn adjust(size: GuiSize) -> GuiSize {
    fn axis(value: u32, min: u32, max: u32) -> u32 {
        let clamped = value.clamp(min, max);
        let rounded = clamped - (clamped % SIZE_QUANTUM);
        rounded.max(min)
    }
    GuiSize {
        width: axis(size.width, MIN_SIZE.width, MAX_SIZE.width),
        height: axis(size.height, MIN_SIZE.height, MAX_SIZE.height),
    }
}

/// The editor's state, owned by the plugin's main-thread half.
pub struct FixtureGui {
    size: GuiSize,
    scale: f64,
    visible: bool,
    created: bool,
    timer: Option<TimerId>,
    /// The `HWND` of the child window, as an integer, or 0 when there is none.
    /// Always 0 on platforms without a backend.
    child: isize,
}

impl Default for FixtureGui {
    fn default() -> Self {
        Self::new()
    }
}

impl FixtureGui {
    pub const fn new() -> Self {
        Self {
            size: PREFERRED_SIZE,
            scale: 1.0,
            visible: false,
            created: false,
            timer: None,
            child: 0,
        }
    }

    /// Win32, embedded. Floating windows are refused: MYDAW embeds, and a fixture
    /// that quietly accepted floating would hide a host bug rather than expose it.
    pub fn is_api_supported(&self, configuration: GuiConfiguration) -> bool {
        backend::AVAILABLE
            && configuration.api_type == GuiApiType::WIN32
            && !configuration.is_floating
    }

    pub fn preferred_api(&self) -> Option<GuiConfiguration<'static>> {
        backend::AVAILABLE.then_some(GuiConfiguration {
            api_type: GuiApiType::WIN32,
            is_floating: false,
        })
    }

    /// Allocate GUI resources. The child window itself is created in
    /// [`set_parent`](Self::set_parent), which is the first moment its parent is
    /// known.
    ///
    /// # Errors
    /// [`PluginError::OperationFailed`] for an API this fixture does not
    /// implement, or if the host asks twice without destroying in between.
    pub fn create(
        &mut self,
        host: &mut HostMainThreadHandle,
        configuration: GuiConfiguration,
    ) -> Result<(), PluginError> {
        if !self.is_api_supported(configuration) || self.created {
            return Err(PluginError::Message(
                "the fixture editor is embedded win32 only, and only one at a time",
            ));
        }
        self.created = true;
        self.size = PREFERRED_SIZE;
        self.visible = false;

        // The animation is driven exclusively by the host's timer, so that a host
        // which does not implement `clap.timer-support` produces a visibly frozen
        // editor rather than one that quietly works for the wrong reason.
        self.timer = host
            .as_shared()
            .as_info()
            .get_extension::<HostTimer>()
            .and_then(|timer| timer.register_timer(host, TIMER_PERIOD_MS).ok());
        Ok(())
    }

    pub fn destroy(&mut self, host: &mut HostMainThreadHandle) {
        if let Some(timer) = self.timer.take() {
            if let Some(ext) = host.as_shared().as_info().get_extension::<HostTimer>() {
                let _ = ext.unregister_timer(host, timer);
            }
        }
        backend::destroy_child(&mut self.child);
        self.created = false;
        self.visible = false;
    }

    /// Whether a host timer was successfully registered. Painted in the editor, so
    /// a host with no timer support is obvious on screen.
    pub const fn has_timer(&self) -> bool {
        self.timer.is_some()
    }

    pub fn set_scale(&mut self, scale: f64) -> Result<(), PluginError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(PluginError::Message(
                "a GUI scale must be finite and positive",
            ));
        }
        self.scale = scale;
        Ok(())
    }

    pub const fn size(&self) -> GuiSize {
        self.size
    }

    pub const fn can_resize(&self) -> bool {
        true
    }

    pub fn resize_hints(&self) -> GuiResizeHints {
        GuiResizeHints {
            can_resize_horizontally: true,
            can_resize_vertically: true,
            strategy: clack_extensions::gui::AspectRatioStrategy::Disregard,
        }
    }

    /// Take a size the host has already negotiated.
    ///
    /// # Errors
    /// [`PluginError::OperationFailed`] if the size is not one
    /// [`adjust`] would have produced -- i.e. if the host skipped the
    /// negotiation. Refusing is legal (`set_size` returns a bool) and is what
    /// makes the fixture useful as a test subject.
    pub fn set_size(&mut self, size: GuiSize) -> Result<(), PluginError> {
        if adjust(size) != size {
            return Err(PluginError::Message(
                "the host set a size it never negotiated through adjust_size",
            ));
        }
        self.size = size;
        backend::resize_child(self.child, size);
        Ok(())
    }

    /// Embed the editor in `window`.
    ///
    /// # Errors
    /// [`PluginError::OperationFailed`] for a non-Win32 handle, or if the window
    /// could not be created.
    pub fn set_parent(&mut self, window: Window) -> Result<(), PluginError> {
        let Some(parent) = window.as_win32_hwnd() else {
            return Err(PluginError::Message(
                "the parent window is not a win32 HWND",
            ));
        };
        backend::destroy_child(&mut self.child);
        self.child = backend::create_child(parent as isize, self.size)?;
        if self.visible {
            backend::set_child_visible(self.child, true);
        }
        Ok(())
    }

    pub fn show(&mut self) -> Result<(), PluginError> {
        self.visible = true;
        backend::set_child_visible(self.child, true);
        Ok(())
    }

    pub fn hide(&mut self) -> Result<(), PluginError> {
        self.visible = false;
        backend::set_child_visible(self.child, false);
        Ok(())
    }

    /// One host timer tick: advance the animation and ask for a repaint.
    ///
    /// Nothing else in this plugin advances the frame counter, which is what makes
    /// the counter a measurement of the host's pump rather than of the plugin.
    pub fn on_timer(&mut self) {
        if self.child != 0 {
            backend::tick(self.child, self.has_timer());
        }
    }
}

#[cfg(windows)]
mod backend {
    //! The Win32 half: one child window that paints a bar and its own state.

    use super::{GuiSize, PluginError};
    use std::ffi::c_void;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::ptr;
    use std::sync::{Once, OnceLock};

    use windows_sys::Win32::Foundation::{
        GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
    };
    use windows_sys::Win32::Foundation::{HINSTANCE, HMODULE};
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, SetBkMode,
        SetTextColor, TextOutW, HBRUSH, PAINTSTRUCT, TRANSPARENT,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
        RegisterClassW, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, CREATESTRUCTW,
        GWLP_USERDATA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, WM_ERASEBKGND,
        WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE, WNDCLASSW, WS_CHILD, WS_VISIBLE,
    };

    /// This platform has an editor.
    pub const AVAILABLE: bool = true;

    /// The registered class name, set once registration succeeds. Carries this
    /// module's handle -- see [`ensure_class`].
    static CLASS_NAME: OnceLock<Vec<u16>> = OnceLock::new();

    /// Background, bar and text colours, as `COLORREF` (0x00BBGGRR).
    const BACKGROUND: COLORREF = 0x0020_1A16;
    const BAR: COLORREF = 0x0018_9CF0;
    const TEXT: COLORREF = 0x00E8_E8E8;

    /// Pixels the bar travels per timer tick.
    const BAR_SPEED: i32 = 6;
    /// Width of the moving bar, in pixels.
    const BAR_WIDTH: i32 = 24;

    static CLASS: Once = Once::new();

    /// Per-window state. Owned by the window, freed on `WM_NCDESTROY`.
    struct ChildState {
        frame: u64,
        has_timer: bool,
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// This **DLL's** module handle -- not the host executable's.
    ///
    /// `GetModuleHandleW(NULL)` returns the handle of the *process*, i.e. the
    /// DAW's `.exe`. A plugin that registers a window class with it registers the
    /// class under the host's identity, and every copy of every plugin bundle in
    /// the process then fights over the same `(name, hInstance)` key. That is a
    /// real crash, not a tidiness point: MYDAW's own test suite reproduced it (two
    /// copies of this bundle loaded from different paths, one unloaded, the other
    /// left creating windows against a window procedure in freed memory).
    ///
    /// Asking for the module that *contains this function* is the correct question
    /// and gives a different handle per loaded copy.
    fn module_handle() -> HINSTANCE {
        let mut handle: HMODULE = ptr::null_mut();
        // SAFETY: `child_proc` is a function in this module, and the out-pointer
        // is a live local. `UNCHANGED_REFCOUNT` means the returned handle must not
        // be freed -- and it is not: it is only used as an identity.
        unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                child_proc as *const () as *const u16,
                &mut handle,
            );
        }
        handle.cast()
    }

    /// Register the editor's window class, once per load of this DLL.
    ///
    /// The class name carries this copy's module handle, so two copies of the
    /// bundle in one process never share a class -- and a class left behind by an
    /// *unloaded* copy can never be picked up by a later one that happens to reuse
    /// the name.
    ///
    /// # Why `ERROR_CLASS_ALREADY_EXISTS` counts as success
    /// Windows does not unregister a DLL's window classes when the DLL is
    /// unloaded, and it usually maps the DLL back to the same base address. The
    /// second load then finds its own class already there. With the module handle
    /// in the name, "already there" can only mean "registered by this same image
    /// at this same address", whose window procedure is exactly the code now
    /// loaded -- so reusing it is correct. Treating it as a failure is how a
    /// plugin ends up working the first time it is loaded and never again.
    fn ensure_class() -> bool {
        CLASS.call_once(|| {
            let instance = module_handle();
            let name = wide(&format!("MydawFixtureGui-{:x}", instance as usize));
            let class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(child_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: ptr::null_mut(),
                hCursor: ptr::null_mut(),
                hbrBackground: ptr::null_mut(),
                lpszMenuName: ptr::null(),
                lpszClassName: name.as_ptr(),
            };
            // SAFETY: `class` is fully initialised and `name` outlives the call;
            // Windows copies both.
            let atom = unsafe { RegisterClassW(&class) };
            // SAFETY: reads this thread's last-error value.
            let already_registered =
                atom == 0 && unsafe { GetLastError() } == ERROR_CLASS_ALREADY_EXISTS;
            if atom != 0 || already_registered {
                let _ = CLASS_NAME.set(name);
            }
        });
        CLASS_NAME.get().is_some()
    }

    pub fn create_child(parent: isize, size: GuiSize) -> Result<isize, PluginError> {
        if parent == 0 || !ensure_class() {
            return Err(PluginError::Message(
                "no parent window, or the editor's window class would not register",
            ));
        }
        let class_name = CLASS_NAME.get().expect("ensure_class just succeeded");
        let state = Box::into_raw(Box::new(ChildState {
            frame: 0,
            has_timer: false,
        }));

        // SAFETY: the class is registered and `state` is a valid pointer the
        // window procedure takes ownership of in WM_NCCREATE.
        let child = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                ptr::null(),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                i32::try_from(size.width).unwrap_or(i32::MAX),
                i32::try_from(size.height).unwrap_or(i32::MAX),
                parent as HWND,
                ptr::null_mut(),
                module_handle(),
                state.cast::<c_void>(),
            )
        };

        if child.is_null() {
            // SAFETY: WM_NCCREATE never ran, so nothing else owns the state.
            drop(unsafe { Box::from_raw(state) });
            return Err(PluginError::Message(
                "CreateWindowExW refused the editor window",
            ));
        }
        Ok(child as isize)
    }

    pub fn destroy_child(child: &mut isize) {
        if *child != 0 {
            // SAFETY: created by `create_child` on this thread.
            unsafe { DestroyWindow(*child as HWND) };
            *child = 0;
        }
    }

    pub fn resize_child(child: isize, size: GuiSize) {
        if child == 0 {
            return;
        }
        // SAFETY: created by `create_child` on this thread.
        unsafe {
            SetWindowPos(
                child as HWND,
                ptr::null_mut(),
                0,
                0,
                i32::try_from(size.width).unwrap_or(i32::MAX),
                i32::try_from(size.height).unwrap_or(i32::MAX),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    pub fn set_child_visible(child: isize, visible: bool) {
        if child == 0 {
            return;
        }
        // SAFETY: created by `create_child` on this thread. `SW_SHOWNA` keeps
        // focus where it is: an editor appearing must not steal the keyboard.
        unsafe { ShowWindow(child as HWND, if visible { SW_SHOWNA } else { SW_HIDE }) };
    }

    /// One host timer tick: advance the frame, republish the state, repaint.
    pub fn tick(child: isize, has_timer: bool) {
        let hwnd = child as HWND;
        // SAFETY: `hwnd` is a live window of our class on this thread; the state
        // reference does not escape this function.
        unsafe {
            let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ChildState;
            let Some(state) = pointer.as_mut() else {
                return;
            };
            state.frame = state.frame.wrapping_add(1);
            state.has_timer = has_timer;

            // The window text is the machine-readable half of this editor: a host
            // test reads it with GetWindowTextW to prove the pump is alive.
            let size = client_size(hwnd);
            let title = wide(&format!(
                "mydaw-fixture-gui {}x{} f={}",
                size.width, size.height, state.frame
            ));
            SetWindowTextW(hwnd, title.as_ptr());
            InvalidateRect(hwnd, ptr::null(), 0);
        }
    }

    /// # Safety
    /// `hwnd` must be a live window on this thread.
    unsafe fn client_size(hwnd: HWND) -> GuiSize {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        GetClientRect(hwnd, &mut rect);
        GuiSize {
            width: (rect.right - rect.left).max(0) as u32,
            height: (rect.bottom - rect.top).max(0) as u32,
        }
    }

    unsafe extern "system" fn child_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // Unwinding into Windows' stack frames is undefined behaviour, and this
        // code runs inside the *host's* message loop: a panic here would take the
        // DAW down, not just the plugin.
        let handled = catch_unwind(AssertUnwindSafe(|| match message {
            WM_NCCREATE => {
                let create = (lparam as *const CREATESTRUCTW).as_ref();
                if let Some(create) = create {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                }
                None
            }
            WM_ERASEBKGND => Some(1),
            WM_SIZE => {
                InvalidateRect(hwnd, ptr::null(), 0);
                Some(0)
            }
            WM_PAINT => {
                paint(hwnd);
                Some(0)
            }
            WM_NCDESTROY => {
                let pointer = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut ChildState;
                if !pointer.is_null() {
                    drop(Box::from_raw(pointer));
                }
                None
            }
            _ => None,
        }))
        .unwrap_or(None);

        match handled {
            Some(result) => result,
            None => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    /// # Safety
    /// Called from the window procedure of a live window of our class.
    unsafe fn paint(hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_null() {
            return;
        }

        let size = client_size(hwnd);
        let width = i32::try_from(size.width).unwrap_or(i32::MAX);
        let height = i32::try_from(size.height).unwrap_or(i32::MAX);
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ChildState;
        let (frame, has_timer) = pointer
            .as_ref()
            .map_or((0, false), |state| (state.frame, state.has_timer));

        let background = CreateSolidBrush(BACKGROUND);
        let full = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        FillRect(hdc, &full, background as HBRUSH);
        DeleteObject(background.cast());

        // The moving bar. Its position depends on nothing but the frame counter,
        // so "is it moving" and "is the host pumping timers" are the same question.
        let travel = width + BAR_WIDTH;
        let offset = if travel > 0 {
            ((frame as i64 * BAR_SPEED as i64) % travel as i64) as i32
        } else {
            0
        };
        let bar_brush = CreateSolidBrush(BAR);
        let bar = RECT {
            left: offset - BAR_WIDTH,
            top: height / 3,
            right: offset,
            bottom: height / 3 + (height / 6).max(8),
        };
        FillRect(hdc, &bar, bar_brush as HBRUSH);
        DeleteObject(bar_brush.cast());

        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, TEXT);
        let line = wide(&format!(
            "MYDAW fixture GUI  {}x{}  frame {}{}",
            size.width,
            size.height,
            frame,
            if has_timer { "" } else { "  (no host timer!)" }
        ));
        // `line` includes the NUL; TextOutW wants the length without it.
        TextOutW(hdc, 12, 12, line.as_ptr(), (line.len() - 1) as i32);

        EndPaint(hwnd, &ps);
    }
}

#[cfg(not(windows))]
mod backend {
    //! No editor on this platform. Every entry point fails cleanly, which is what
    //! a host must cope with anyway.

    use super::{GuiSize, PluginError};

    pub const AVAILABLE: bool = false;

    pub fn create_child(_parent: isize, _size: GuiSize) -> Result<isize, PluginError> {
        Err(PluginError::Message(
            "the fixture editor is implemented for Windows only",
        ))
    }

    pub fn destroy_child(_child: &mut isize) {}
    pub fn resize_child(_child: isize, _size: GuiSize) {}
    pub fn set_child_visible(_child: isize, _visible: bool) {}
    pub fn tick(_child: isize, _has_timer: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preferred_size_is_one_the_plugin_would_accept() {
        assert_eq!(adjust(PREFERRED_SIZE), PREFERRED_SIZE);
    }

    #[test]
    fn adjust_rounds_down_to_the_quantum() {
        assert_eq!(
            adjust(GuiSize {
                width: 517,
                height: 333
            }),
            GuiSize {
                width: 510,
                height: 330
            }
        );
        // Never larger than requested: CLAP requires it.
        for width in [201, 209, 210, 999] {
            let adjusted = adjust(GuiSize { width, height: 500 });
            assert!(adjusted.width <= width, "{adjusted:?} exceeds {width}");
        }
    }

    #[test]
    fn adjust_clamps_to_the_supported_range() {
        assert_eq!(
            adjust(GuiSize {
                width: 1,
                height: 1
            }),
            MIN_SIZE
        );
        assert_eq!(
            adjust(GuiSize {
                width: 100_000,
                height: 100_000
            }),
            MAX_SIZE
        );
    }

    #[test]
    fn a_size_the_host_did_not_negotiate_is_refused() {
        let mut gui = FixtureGui::new();
        // 517x333 is not a multiple of the quantum: the host skipped adjust_size.
        assert!(gui
            .set_size(GuiSize {
                width: 517,
                height: 333
            })
            .is_err());
        assert_eq!(gui.size(), PREFERRED_SIZE);

        gui.set_size(GuiSize {
            width: 510,
            height: 330,
        })
        .unwrap();
        assert_eq!(
            gui.size(),
            GuiSize {
                width: 510,
                height: 330
            }
        );
    }

    #[test]
    fn only_embedded_win32_is_supported() {
        let gui = FixtureGui::new();
        let embedded = GuiConfiguration {
            api_type: GuiApiType::WIN32,
            is_floating: false,
        };
        let floating = GuiConfiguration {
            api_type: GuiApiType::WIN32,
            is_floating: true,
        };
        let cocoa = GuiConfiguration {
            api_type: GuiApiType::COCOA,
            is_floating: false,
        };
        assert_eq!(gui.is_api_supported(embedded), cfg!(windows));
        assert!(!gui.is_api_supported(floating));
        assert!(!gui.is_api_supported(cocoa));
    }

    #[test]
    fn scale_rejects_values_that_are_not_scales() {
        let mut gui = FixtureGui::new();
        assert!(gui.set_scale(1.5).is_ok());
        assert!(gui.set_scale(0.0).is_err());
        assert!(gui.set_scale(-1.0).is_err());
        assert!(gui.set_scale(f64::NAN).is_err());
        assert!(gui.set_scale(f64::INFINITY).is_err());
    }

    #[test]
    fn a_fresh_editor_has_no_window_and_no_timer() {
        let gui = FixtureGui::new();
        assert!(!gui.has_timer());
        assert_eq!(gui.child, 0);
        assert!(gui.can_resize());
        assert!(gui.resize_hints().can_resize_horizontally);
    }
}
