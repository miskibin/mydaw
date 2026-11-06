//! The Win32 backend: the only file in the crate that knows what an `HWND` is.
//!
//! # Why raw Win32 and not `tao`
//! `tao` can only create windows from inside its own event loop, through an
//! `EventLoopWindowTarget` Tauri does not hand out where an editor is opened.
//! Plain Win32 windows created **on the same thread** as tao's loop need no
//! such handle: tao's `GetMessageW(hwnd = NULL)` + `DispatchMessageW` delivers
//! to every window of the thread, ours and the plugin's child alike.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Once};

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetStockObject, BLACK_BRUSH, HBRUSH};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindow, GetWindowLongPtrW,
    IsWindow, KillTimer, RegisterClassW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, GW_CHILD, MINMAXINFO,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOWNORMAL, USER_DEFAULT_SCREEN_DPI,
    WINDOW_EX_STYLE, WM_CLOSE, WM_DPICHANGED, WM_GETMINMAXINFO, WM_NCCREATE, WM_NCDESTROY, WM_SIZE,
    WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CLIPCHILDREN, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU, WS_THICKFRAME,
};

use crate::service::{
    WindowError, WindowEvent, WindowId, WindowOptions, WindowSize, WindowState, MIN_WINDOW_SIZE,
    PUMP_INTERVAL_MS,
};

/// Window class name, unique to WAVE so it cannot collide with a plugin's own.
const CLASS_NAME: &str = "WavePluginWindow";

/// Id of the wake-up timer. Plugin timers live on the plugin's own windows, so
/// collision is impossible.
const PUMP_TIMER_ID: usize = 0xDA11;

const NO_EX_STYLE: WINDOW_EX_STYLE = 0;

static CLASS: Once = Once::new();
/// `0` = registered, anything else = the `GetLastError` from the attempt.
static CLASS_ERROR: AtomicU32 = AtomicU32::new(u32::MAX);

const STYLE_RESIZABLE: u32 = WS_OVERLAPPED
    | WS_CAPTION
    | WS_SYSMENU
    | WS_THICKFRAME
    | WS_MINIMIZEBOX
    | WS_MAXIMIZEBOX
    | WS_CLIPCHILDREN;

/// No drag frame, no maximise box: an editor that cannot resize must not be
/// given a frame the user can drag.
const STYLE_FIXED: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;

fn style_for(resizable: bool) -> u32 {
    if resizable {
        STYLE_RESIZABLE
    } else {
        STYLE_FIXED
    }
}

/// UTF-16, NUL-terminated.
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: no preconditions; reads this thread's last-error value.
    unsafe { GetLastError() }
}

/// `GWL_STYLE` is not exported by `windows-sys` under a usable name here.
const GWL_STYLE: windows_sys::Win32::UI::WindowsAndMessaging::WINDOW_LONG_PTR_INDEX = -16;

fn ensure_class() -> Result<(), WindowError> {
    CLASS.call_once(|| {
        let class_name = wide(CLASS_NAME);
        // SAFETY: `GetModuleHandleW(NULL)` returns this executable's instance
        // handle and cannot fail for the current process.
        let instance = unsafe { GetModuleHandleW(ptr::null()) };
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            // SAFETY: `BLACK_BRUSH` is a valid stock-object index; the system
            // owns the brush.
            hbrBackground: unsafe { GetStockObject(BLACK_BRUSH) } as HBRUSH,
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        // SAFETY: `class` and the name it points at outlive the call; Windows
        // copies both.
        let atom = unsafe { RegisterClassW(&class) };
        CLASS_ERROR.store(if atom == 0 { last_error() } else { 0 }, Ordering::Release);
    });
    match CLASS_ERROR.load(Ordering::Acquire) {
        0 => Ok(()),
        code => Err(WindowError::ClassRegistration(code)),
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
    events: Sender<WindowEvent>,
    ticks: Arc<AtomicU64>,
) -> Result<isize, WindowError> {
    ensure_class()?;
    let style = style_for(options.resizable);
    // The real DPI is unknown until the window exists (it depends on the
    // monitor), so create at the default scale and correct immediately after.
    let rect = window_rect_for(size, style, USER_DEFAULT_SCREEN_DPI);
    let title = wide(&options.title);
    let class_name = wide(CLASS_NAME);

    let state = Box::into_raw(Box::new(WindowState {
        events,
        ticks,
        // The service's floor, not the editor's initial size: a resizable editor
        // may be dragged smaller than it opened.
        min_size: MIN_WINDOW_SIZE,
        tick: std::cell::RefCell::new(None),
    }));

    // SAFETY: the class is registered, the strings are NUL-terminated and live
    // across the call, and `state` is a valid pointer whose ownership the window
    // procedure takes in WM_NCCREATE.
    let hwnd = unsafe {
        CreateWindowExW(
            NO_EX_STYLE,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            ptr::null_mut(),
            ptr::null_mut(),
            GetModuleHandleW(ptr::null()),
            state.cast::<c_void>(),
        )
    };
    if hwnd.is_null() {
        let code = last_error();
        // SAFETY: WM_NCCREATE never ran, so the state was never adopted.
        drop(unsafe { Box::from_raw(state) });
        return Err(WindowError::Creation(code));
    }

    // SAFETY: `hwnd` is a window we just created on this thread.
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi != USER_DEFAULT_SCREEN_DPI {
            apply_client_size(hwnd, size, style, dpi);
        }
        SetTimer(hwnd, PUMP_TIMER_ID, PUMP_INTERVAL_MS, None);
    }
    Ok(hwnd as isize)
}

/// Whether `hwnd` is a live window (0 is the portable layer's "no window").
fn alive(hwnd: isize) -> bool {
    // SAFETY: `IsWindow` accepts any value and only answers.
    hwnd != 0 && unsafe { IsWindow(hwnd as HWND) } != 0
}

pub(crate) fn destroy_window(hwnd: isize) {
    if !alive(hwnd) {
        return;
    }
    let hwnd = hwnd as HWND;
    // SAFETY: created by this module on this thread; DestroyWindow frees the
    // boxed state through WM_NCDESTROY.
    unsafe {
        KillTimer(hwnd, PUMP_TIMER_ID);
        DestroyWindow(hwnd);
    }
}

pub(crate) fn show_window(hwnd: isize) {
    if alive(hwnd) {
        // SAFETY: created by this module on this thread.
        unsafe { ShowWindow(hwnd as HWND, SW_SHOWNORMAL) };
    }
}

pub(crate) fn request_close(hwnd: isize) {
    if !alive(hwnd) {
        return;
    }
    // SAFETY: a window of this class on this thread; the state pointer is valid
    // until WM_NCDESTROY, and `run_tick` keeps the handler alive on its own.
    unsafe {
        if let Some(state) = state_of(hwnd as HWND) {
            state.post(WindowEvent::Closed);
            state.run_tick();
        }
    }
}

pub(crate) fn set_tick_handler(hwnd: isize, handler: Option<Rc<dyn Fn()>>) {
    if !alive(hwnd) {
        return;
    }
    // SAFETY: as in `request_close`.
    unsafe {
        if let Some(state) = state_of(hwnd as HWND) {
            *state.tick.borrow_mut() = handler;
        }
    }
}

pub(crate) fn dpi(hwnd: isize) -> u32 {
    // SAFETY: returns 0 for an invalid window, mapped to the default scale.
    let dpi = unsafe { GetDpiForWindow(hwnd as HWND) };
    if dpi == 0 {
        USER_DEFAULT_SCREEN_DPI
    } else {
        dpi
    }
}

pub(crate) fn has_child_window(hwnd: isize) -> Result<bool, WindowError> {
    // SAFETY: created by this module on this thread.
    let child = unsafe { GetWindow(hwnd as HWND, GW_CHILD) };
    Ok(!child.is_null())
}

pub(crate) fn client_size(id: WindowId, hwnd: isize) -> Result<WindowSize, WindowError> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: created by this module on this thread; `rect` is ours.
    if unsafe { GetClientRect(hwnd as HWND, &mut rect) } == 0 {
        return Err(WindowError::Win32 {
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

pub(crate) fn set_client_size(id: WindowId, hwnd: isize, size: WindowSize) -> Result<(), WindowError> {
    let hwnd = hwnd as HWND;
    // SAFETY: created by this module on this thread.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    let dpi = dpi(hwnd as isize);
    // SAFETY: as above.
    if unsafe { apply_client_size(hwnd, size, style, dpi) } {
        Ok(())
    } else {
        Err(WindowError::Win32 {
            id,
            api: "SetWindowPos",
            code: last_error(),
        })
    }
}

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

/// The state a window carries. `None` before WM_NCCREATE and after WM_NCDESTROY.
///
/// # Safety
/// `hwnd` must be a window of this class, on this thread.
unsafe fn state_of<'a>(hwnd: HWND) -> Option<&'a WindowState> {
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowState;
    pointer.as_ref()
}

/// The window procedure. Runs inside the UI loop's `DispatchMessageW`.
///
/// It queues events and returns; the only owner code it runs is the tick
/// handler, at `WM_TIMER`, which is always dispatched from the top of the loop.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // A panic unwinding into Windows' frames is undefined behaviour.
    let handled =
        catch_unwind(AssertUnwindSafe(|| dispatch(hwnd, message, wparam, lparam))).unwrap_or(None);
    match handled {
        Some(result) => result,
        None => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

/// # Safety
/// Called only from [`window_proc`], with its arguments.
unsafe fn dispatch(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match message {
        WM_NCCREATE => {
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
            state.post(WindowEvent::Resized(WindowSize::new(width, height)));
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
            // Taking the suggested rectangle verbatim keeps the window the same
            // physical size as it crosses monitors.
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
            state.post(WindowEvent::DpiChanged { dpi: new_dpi });
            Some(0)
        }
        WM_TIMER if wparam == PUMP_TIMER_ID => {
            if let Some(state) = state_of(hwnd) {
                state.ticks.fetch_add(1, Ordering::Relaxed);
                // May destroy this window; nothing below touches `state`.
                state.run_tick();
            }
            Some(0)
        }
        WM_CLOSE => {
            // Report, do not destroy: the editor must be detached first.
            if let Some(state) = state_of(hwnd) {
                state.post(WindowEvent::Closed);
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
        assert_eq!(wide("é").len(), 2);
    }

    #[test]
    fn a_fixed_window_has_no_drag_frame_and_a_resizable_one_does() {
        assert_eq!(style_for(true) & WS_THICKFRAME, WS_THICKFRAME);
        assert_eq!(style_for(false) & WS_THICKFRAME, 0);
        assert_eq!(style_for(false) & WS_MAXIMIZEBOX, 0);
        assert_eq!(style_for(true) & WS_CLIPCHILDREN, WS_CLIPCHILDREN);
        assert_eq!(style_for(false) & WS_CLIPCHILDREN, WS_CLIPCHILDREN);
    }

    #[test]
    fn the_window_rectangle_is_larger_than_the_client_area_it_wraps() {
        let rect = window_rect_for(WindowSize::new(400, 250), style_for(true), USER_DEFAULT_SCREEN_DPI);
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        assert!(width >= 400 && height >= 250, "got {width}x{height}");
        assert!(height - 250 < 200, "implausible caption height");
    }
}
