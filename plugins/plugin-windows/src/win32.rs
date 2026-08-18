//! The Win32 backend: the only file in the crate that knows what an `HWND` is.
//!
//! # Why raw Win32 and not `tao`
//! `tao` (the windowing half of Tauri) can only create windows from inside its
//! own event loop, through an `EventLoopWindowTarget` that Tauri does not hand
//! out at the point a plugin editor is opened. Rather than restructure the shell
//! around that, this backend creates plain Win32 windows **on the same thread**
//! as tao's loop.
//!
//! That works because a Win32 message queue belongs to a *thread*, not to a
//! library: tao's loop calls `GetMessageW(hwnd = NULL)` and `DispatchMessageW`,
//! which delivers to every window created on that thread, including these. No
//! integration, no second loop, no cross-thread marshalling -- and the plugin's
//! own child window, created by the vendor on the same thread during
//! `clap_plugin_gui->create()`, is pumped by exactly the same loop.
//!
//! The claim is verified rather than assumed: `tests/native_window.rs` runs a
//! *foreign* message loop over windows this module created, and the spike's smoke
//! test watches an animated plugin GUI driven only by host timers.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Once};

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetStockObject, BLACK_BRUSH, HBRUSH};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindow, GetWindowLongPtrW,
    KillTimer, RegisterClassW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, GW_CHILD, MINMAXINFO,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOWNORMAL, USER_DEFAULT_SCREEN_DPI,
    WINDOW_EX_STYLE, WM_CLOSE, WM_DPICHANGED, WM_GETMINMAXINFO, WM_NCCREATE, WM_NCDESTROY, WM_SIZE,
    WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CLIPCHILDREN, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU, WS_THICKFRAME,
};

use crate::service::{
    PluginWindowError, PluginWindowEvent, PluginWindowId, WindowOptions, WindowSize, WindowState,
    MIN_WINDOW_SIZE, PUMP_INTERVAL_MS,
};

/// Window class name. Unique to MYDAW so it cannot collide with a plugin's own.
const CLASS_NAME: &[u16] = &[
    b'M' as u16,
    b'y' as u16,
    b'd' as u16,
    b'a' as u16,
    b'w' as u16,
    b'P' as u16,
    b'l' as u16,
    b'u' as u16,
    b'g' as u16,
    b'i' as u16,
    b'n' as u16,
    b'W' as u16,
    b'i' as u16,
    b'n' as u16,
    b'd' as u16,
    b'o' as u16,
    b'w' as u16,
    0,
];

/// Id of the wake-up timer. Arbitrary, but must not collide with a plugin's
/// timers -- those run on the plugin's own windows, so collision is impossible.
const PUMP_TIMER_ID: usize = 0xDA11;

/// No extended window style. Spelled out because `0` on its own reads as a
/// forgotten argument.
const NO_EX_STYLE: WINDOW_EX_STYLE = 0;

/// Class registration happens once per process.
static CLASS: Once = Once::new();
/// `0` = registered, anything else = the `GetLastError` from the attempt.
static CLASS_ERROR: AtomicU32 = AtomicU32::new(u32::MAX);

/// Style of a resizable window.
const STYLE_RESIZABLE: u32 = WS_OVERLAPPED
    | WS_CAPTION
    | WS_SYSMENU
    | WS_THICKFRAME
    | WS_MINIMIZEBOX
    | WS_MAXIMIZEBOX
    | WS_CLIPCHILDREN;

/// Style of a fixed-size window: no drag frame, no maximise box. A plugin that
/// answers `can_resize() == false` must not be given a frame the user can drag.
const STYLE_FIXED: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;

fn style_for(resizable: bool) -> u32 {
    if resizable {
        STYLE_RESIZABLE
    } else {
        STYLE_FIXED
    }
}

/// UTF-16, NUL-terminated. Win32 wants wide strings; Rust strings are not.
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: no preconditions; reads this thread's last-error value.
    unsafe { GetLastError() }
}

/// Register the window class, once.
fn ensure_class() -> Result<(), PluginWindowError> {
    CLASS.call_once(|| {
        // SAFETY: `GetModuleHandleW(NULL)` returns this module's instance handle
        // and cannot fail for the current process.
        let instance = unsafe { GetModuleHandleW(ptr::null()) };
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance.cast(),
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            // Stock object: owned by the system, never destroyed by us.
            // SAFETY: `BLACK_BRUSH` is a valid stock-object index.
            hbrBackground: unsafe { GetStockObject(BLACK_BRUSH) } as HBRUSH,
            lpszMenuName: ptr::null(),
            lpszClassName: CLASS_NAME.as_ptr(),
        };
        // SAFETY: `class` is fully initialised and outlives the call; Windows
        // copies it.
        let atom = unsafe { RegisterClassW(&class) };
        CLASS_ERROR.store(if atom == 0 { last_error() } else { 0 }, Ordering::Release);
    });

    match CLASS_ERROR.load(Ordering::Acquire) {
        0 => Ok(()),
        code => Err(PluginWindowError::ClassRegistration(code)),
    }
}

/// The outer window rectangle that yields `size` as the client area at `dpi`.
fn window_rect_for(size: WindowSize, style: u32, dpi: u32) -> RECT {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: i32::try_from(size.width).unwrap_or(i32::MAX),
        bottom: i32::try_from(size.height).unwrap_or(i32::MAX),
    };
    // SAFETY: `rect` is a valid, initialised RECT we own.
    unsafe {
        AdjustWindowRectExForDpi(&mut rect, style, 0, NO_EX_STYLE, dpi);
    }
    rect
}

pub(crate) fn create_window(
    options: &WindowOptions,
    size: WindowSize,
    events: Sender<PluginWindowEvent>,
    ticks: Arc<AtomicU64>,
) -> Result<isize, PluginWindowError> {
    ensure_class()?;

    let style = style_for(options.resizable);
    // The real DPI is unknown until the window exists (it depends on which
    // monitor Windows puts it on), so create at the default scale and correct the
    // client size immediately afterwards.
    let rect = window_rect_for(size, style, USER_DEFAULT_SCREEN_DPI);
    let title = wide(&options.title);

    let state = Box::new(WindowState {
        events,
        ticks,
        // The floor is the service's, not the plugin's initial size: a resizable
        // editor is allowed to be dragged smaller than it opened, and the plugin
        // gets the final say through `adjust_size` anyway. What the floor
        // prevents is a window with no room for its own close button.
        min_size: MIN_WINDOW_SIZE,
    });
    // Ownership passes to the window, which frees it on WM_NCDESTROY.
    let state = Box::into_raw(state);

    // SAFETY: the class is registered, the strings are NUL-terminated and live
    // across the call, and `state` is a valid pointer whose ownership the window
    // procedure takes in WM_NCCREATE.
    let hwnd = unsafe {
        CreateWindowExW(
            NO_EX_STYLE,
            CLASS_NAME.as_ptr(),
            title.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            ptr::null_mut(),
            ptr::null_mut(),
            GetModuleHandleW(ptr::null()).cast(),
            state.cast::<c_void>(),
        )
    };

    if hwnd.is_null() {
        let code = last_error();
        // The window never existed, so nothing will ever free the state.
        // SAFETY: `state` came from `Box::into_raw` and was not consumed --
        // WM_NCCREATE never ran.
        drop(unsafe { Box::from_raw(state) });
        return Err(PluginWindowError::Creation(code));
    }

    // SAFETY: `hwnd` is a window we just created on this thread.
    unsafe {
        // Re-apply the client size at the monitor's real DPI. On a 100% display
        // this is a no-op; on a 150% one it is the difference between a plugin
        // editor that fits and one that is clipped.
        let dpi = GetDpiForWindow(hwnd);
        if dpi != USER_DEFAULT_SCREEN_DPI {
            apply_client_size(hwnd, size, style, dpi);
        }
        // The wake-up timer: its only purpose is to make the host's GetMessage
        // return, so the host can service CLAP timers and main-thread callbacks.
        SetTimer(hwnd, PUMP_TIMER_ID, PUMP_INTERVAL_MS, None);
    }

    Ok(hwnd as isize)
}

pub(crate) fn destroy_window(hwnd: isize) {
    let hwnd = hwnd as HWND;
    // SAFETY: `hwnd` was created by this module on this thread; KillTimer on an
    // unknown id is harmless, and DestroyWindow frees the boxed state through
    // WM_NCDESTROY.
    unsafe {
        KillTimer(hwnd, PUMP_TIMER_ID);
        DestroyWindow(hwnd);
    }
}

pub(crate) fn show_window(hwnd: isize) {
    // SAFETY: `hwnd` was created by this module on this thread.
    unsafe { ShowWindow(hwnd as HWND, SW_SHOWNORMAL) };
}

pub(crate) fn dpi(hwnd: isize) -> u32 {
    // SAFETY: `hwnd` was created by this module on this thread. Returns 0 for an
    // invalid window, which the caller would rather see as "the default scale".
    let dpi = unsafe { GetDpiForWindow(hwnd as HWND) };
    if dpi == 0 {
        USER_DEFAULT_SCREEN_DPI
    } else {
        dpi
    }
}

pub(crate) fn has_child_window(hwnd: isize) -> Result<bool, PluginWindowError> {
    // SAFETY: `hwnd` was created by this module on this thread.
    let child = unsafe { GetWindow(hwnd as HWND, GW_CHILD) };
    Ok(!child.is_null())
}

pub(crate) fn client_size(
    id: PluginWindowId,
    hwnd: isize,
) -> Result<WindowSize, PluginWindowError> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `hwnd` was created by this module on this thread; `rect` is ours.
    let ok = unsafe { GetClientRect(hwnd as HWND, &mut rect) };
    if ok == 0 {
        return Err(PluginWindowError::Win32 {
            id,
            api: "GetClientRect",
            code: last_error(),
        });
    }
    Ok(WindowSize {
        width: (rect.right - rect.left).max(0) as u32,
        height: (rect.bottom - rect.top).max(0) as u32,
    })
}

pub(crate) fn set_client_size(
    id: PluginWindowId,
    hwnd: isize,
    size: WindowSize,
) -> Result<(), PluginWindowError> {
    let hwnd = hwnd as HWND;
    // SAFETY: `hwnd` was created by this module on this thread.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    let dpi = dpi(hwnd as isize);
    // SAFETY: as above.
    let ok = unsafe { apply_client_size(hwnd, size, style, dpi) };
    if ok {
        Ok(())
    } else {
        Err(PluginWindowError::Win32 {
            id,
            api: "SetWindowPos",
            code: last_error(),
        })
    }
}

/// `GWL_STYLE` lives in a different constant family than the `GWLP_*` ones and is
/// not exported by `windows-sys` under a name usable here.
const GWL_STYLE: windows_sys::Win32::UI::WindowsAndMessaging::WINDOW_LONG_PTR_INDEX = -16;

/// Resize `hwnd` so its client area is exactly `size`, keeping its position.
///
/// # Safety
/// `hwnd` must be a live window owned by the calling thread.
unsafe fn apply_client_size(hwnd: HWND, size: WindowSize, style: u32, dpi: u32) -> bool {
    let rect = window_rect_for(size, style, dpi);
    SetWindowPos(
        hwnd,
        ptr::null_mut(),
        0,
        0,
        rect.right - rect.left,
        rect.bottom - rect.top,
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
    ) != 0
}

/// Read the state pointer a window carries. `None` before WM_NCCREATE and after
/// WM_NCDESTROY.
///
/// # Safety
/// `hwnd` must be a window of this class, on this thread.
unsafe fn state_of<'a>(hwnd: HWND) -> Option<&'a WindowState> {
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowState;
    pointer.as_ref()
}

/// The window procedure. Runs inside the *host's* `DispatchMessageW`.
///
/// Everything it does is cheap and non-blocking: it pushes an event into a
/// channel and returns. No host code, no CLAP call and no lock that another
/// thread can hold ever runs here -- a window procedure that blocks freezes the
/// whole application, including the webview.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // A panic unwinding into Windows' own stack frames is undefined behaviour.
    // Nothing below should panic; if the impossible happens, fall through to the
    // default handler rather than take the process with us.
    let handled =
        catch_unwind(AssertUnwindSafe(|| dispatch(hwnd, message, wparam, lparam))).unwrap_or(None);

    match handled {
        Some(result) => result,
        None => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

/// The half of [`window_proc`] that is allowed to be readable.
///
/// Returns `Some(result)` for messages this window handles, `None` to defer to
/// `DefWindowProcW`.
///
/// # Safety
/// Called only from [`window_proc`], with its arguments.
unsafe fn dispatch(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match message {
        WM_NCCREATE => {
            // Take ownership of the state the creator boxed for us.
            let create = lparam as *const CREATESTRUCTW;
            if let Some(create) = create.as_ref() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            None
        }
        WM_SIZE => {
            let state = state_of(hwnd)?;
            let width = (lparam as u32) & 0xFFFF;
            let height = ((lparam as u32) >> 16) & 0xFFFF;
            state.post(PluginWindowEvent::Resized(WindowSize::new(width, height)));
            Some(0)
        }
        WM_GETMINMAXINFO => {
            let state = state_of(hwnd)?;
            let info = (lparam as *mut MINMAXINFO).as_mut()?;
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let rect = window_rect_for(state.min_size, style, dpi(hwnd as isize));
            info.ptMinTrackSize = POINT {
                x: rect.right - rect.left,
                y: rect.bottom - rect.top,
            };
            Some(0)
        }
        WM_DPICHANGED => {
            let state = state_of(hwnd)?;
            let new_dpi = (wparam as u32) & 0xFFFF;
            // Windows hands us the rectangle the window should move to. Taking it
            // verbatim is what keeps a window the same *physical* size as it
            // crosses monitors.
            if let Some(suggested) = (lparam as *const RECT).as_ref() {
                SetWindowPos(
                    hwnd,
                    ptr::null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            state.post(PluginWindowEvent::DpiChanged { dpi: new_dpi });
            Some(0)
        }
        WM_TIMER if wparam == PUMP_TIMER_ID => {
            if let Some(state) = state_of(hwnd) {
                state.ticks.fetch_add(1, Ordering::Relaxed);
            }
            Some(0)
        }
        WM_CLOSE => {
            // Report, do not destroy: the owner must tear the plugin's GUI down
            // before its parent window disappears.
            if let Some(state) = state_of(hwnd) {
                state.post(PluginWindowEvent::Closed);
            }
            Some(0)
        }
        WM_NCDESTROY => {
            let pointer = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut WindowState;
            if !pointer.is_null() {
                drop(Box::from_raw(pointer));
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_are_nul_terminated() {
        assert_eq!(wide("hi"), vec![b'h' as u16, b'i' as u16, 0]);
        assert_eq!(wide(""), vec![0]);
        // Non-ASCII survives: a vendor's plugin name is not necessarily ASCII.
        assert_eq!(wide("é").len(), 2);
    }

    #[test]
    fn the_class_name_is_a_valid_wide_string() {
        assert_eq!(*CLASS_NAME.last().unwrap(), 0);
        let name = String::from_utf16(&CLASS_NAME[..CLASS_NAME.len() - 1]).unwrap();
        assert_eq!(name, "MydawPluginWindow");
    }

    #[test]
    fn a_fixed_window_has_no_drag_frame_and_a_resizable_one_does() {
        assert_eq!(style_for(true) & WS_THICKFRAME, WS_THICKFRAME);
        assert_eq!(style_for(false) & WS_THICKFRAME, 0);
        assert_eq!(style_for(false) & WS_MAXIMIZEBOX, 0);
        // Both clip children, so the plugin's child window is not painted over.
        assert_eq!(style_for(true) & WS_CLIPCHILDREN, WS_CLIPCHILDREN);
        assert_eq!(style_for(false) & WS_CLIPCHILDREN, WS_CLIPCHILDREN);
    }

    #[test]
    fn the_window_rectangle_is_larger_than_the_client_area_it_wraps() {
        let client = WindowSize::new(400, 250);
        let rect = window_rect_for(client, style_for(true), USER_DEFAULT_SCREEN_DPI);
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        assert!(
            width >= 400 && height >= 250,
            "a titled window is bigger than its client area, got {width}x{height}"
        );
        // A caption is tens of pixels, not hundreds.
        assert!(height - 250 < 200, "implausible caption height");
    }

    #[test]
    fn a_higher_dpi_asks_for_thicker_decorations() {
        let client = WindowSize::new(400, 250);
        let normal = window_rect_for(client, style_for(true), USER_DEFAULT_SCREEN_DPI);
        let dense = window_rect_for(client, style_for(true), USER_DEFAULT_SCREEN_DPI * 2);
        assert!(
            (dense.bottom - dense.top) > (normal.bottom - normal.top),
            "decorations scale with DPI"
        );
    }
}
