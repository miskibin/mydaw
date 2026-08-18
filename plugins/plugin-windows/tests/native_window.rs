//! Real windows, driven by a *foreign* message loop.
//!
//! This is the test that decides the spike's architecture. The premise of
//! `mydaw-plugin-windows` is that a window created by us is pumped by whatever
//! message loop already runs on the thread -- in the application, tao's. The loop
//! below is not ours either: it is a plain `PeekMessage`/`DispatchMessage` pair
//! written here, with no knowledge of the service, standing in for tao's.
//!
//! If these pass, `WM_SIZE`, `WM_CLOSE` and `WM_TIMER` reach our window procedure
//! through a loop that knows nothing about it, which is exactly the claim.
//!
//! Requires a window station (any interactive Windows session, and GitHub's
//! `windows-latest` runners). Nothing is ever shown: the windows stay hidden
//! unless a test asks for `show_window`.

#![cfg(windows)]

use std::time::{Duration, Instant};

use mydaw_plugin_windows::{
    PluginWindowError, PluginWindowEvent, PluginWindowService, WindowOptions, WindowSize,
};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, IsWindow, PeekMessageW, PostMessageW, TranslateMessage, MSG, PM_REMOVE,
    WM_CLOSE,
};

/// The stand-in for tao's loop: drain this thread's message queue for `duration`.
fn pump(duration: Duration) {
    let deadline = Instant::now() + duration;
    let mut message = MSG {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
    };
    while Instant::now() < deadline {
        // SAFETY: `message` is a live, owned MSG; both calls are documented to
        // take a pointer to one.
        unsafe {
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Pump until `condition` holds or `timeout` expires. Returns whether it held.
fn pump_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        pump(Duration::from_millis(10));
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
    }
}

fn is_window(hwnd: isize) -> bool {
    // SAFETY: `IsWindow` accepts any value, including a stale handle -- that is
    // the entire point of asking it.
    unsafe { IsWindow(hwnd as HWND) != 0 }
}

#[test]
fn a_foreign_message_loop_pumps_our_window() {
    let service = PluginWindowService::new();
    let options =
        WindowOptions::new("MYDAW test window", WindowSize::new(400, 250)).resizable(true);
    let window = service.open_window(&options).expect("window must open");

    assert_ne!(window.hwnd(), 0);
    assert!(is_window(window.hwnd()));
    assert!(service.is_open(window.id()));
    assert_eq!(service.open_count(), 1);

    // The client area is what we asked for, at whatever DPI the window landed on.
    assert_eq!(
        service.client_size(window.id()).unwrap(),
        WindowSize::new(400, 250)
    );
    assert!(service.dpi(window.id()).unwrap() >= 96);

    // The wake-up timer is the load-bearing part: a host loop that never wakes
    // cannot service CLAP timers or `on_main_thread` callbacks.
    assert!(
        pump_until(Duration::from_secs(2), || window.pump_ticks() > 0),
        "no WM_TIMER reached the window through a foreign message loop"
    );

    service.close_window(window.id()).unwrap();
    assert!(!service.is_open(window.id()));
    assert!(!is_window(window.hwnd()), "the window must really be gone");
}

#[test]
fn resizing_reports_the_new_client_size() {
    let service = PluginWindowService::new();
    let options = WindowOptions::new("MYDAW resize", WindowSize::new(400, 250)).resizable(true);
    let window = service.open_window(&options).expect("window must open");

    // Whatever WM_SIZE arrived during creation is not what this test is about.
    pump(Duration::from_millis(30));
    let _ = window.drain_events();

    service
        .set_client_size(window.id(), WindowSize::new(640, 480))
        .unwrap();

    let mut resized = None;
    let seen = pump_until(Duration::from_secs(2), || {
        for event in window.drain_events() {
            if let PluginWindowEvent::Resized(size) = event {
                resized = Some(size);
            }
        }
        resized == Some(WindowSize::new(640, 480))
    });
    assert!(seen, "expected a Resized(640x480) event, saw {resized:?}");
    assert_eq!(
        service.client_size(window.id()).unwrap(),
        WindowSize::new(640, 480)
    );

    // Below the service's floor the request is clamped, not obeyed: a plugin
    // asking for a 1x1 editor still gets a usable window.
    service
        .set_client_size(window.id(), WindowSize::new(1, 1))
        .unwrap();
    pump(Duration::from_millis(50));
    let clamped = service.client_size(window.id()).unwrap();
    assert_eq!(clamped, WindowSize::new(1, 1).clamped());

    service.close_window(window.id()).unwrap();
}

#[test]
fn the_close_button_reports_but_does_not_destroy() {
    let service = PluginWindowService::new();
    let options = WindowOptions::new("MYDAW close", WindowSize::new(320, 200));
    let window = service.open_window(&options).expect("window must open");

    // SAFETY: posting to a live window we own.
    unsafe { PostMessageW(window.hwnd() as HWND, WM_CLOSE, 0, 0) };

    let mut closed = false;
    let seen = pump_until(Duration::from_secs(2), || {
        closed |= window.drain_events().contains(&PluginWindowEvent::Closed);
        closed
    });
    assert!(seen, "WM_CLOSE must be reported to the owner");
    assert!(
        is_window(window.hwnd()),
        "the window must survive its close button: a CLAP GUI has to be \
         destroyed before the window it lives in"
    );
    assert!(service.is_open(window.id()));

    service.close_window(window.id()).unwrap();
    assert!(!is_window(window.hwnd()));
}

#[test]
fn a_window_can_be_shown_and_closed_without_a_visible_regression() {
    let service = PluginWindowService::new();
    let options = WindowOptions::new("MYDAW show", WindowSize::new(300, 180));
    let window = service.open_window(&options).expect("window must open");

    service.show_window(window.id()).unwrap();
    pump(Duration::from_millis(50));
    assert!(is_window(window.hwnd()));

    service.close_window(window.id()).unwrap();
    // Every call against a closed window is a typed error, not a crash.
    assert!(matches!(
        service.close_window(window.id()),
        Err(PluginWindowError::UnknownWindow(_))
    ));
    assert!(matches!(
        service.set_client_size(window.id(), WindowSize::new(200, 200)),
        Err(PluginWindowError::UnknownWindow(_))
    ));
}

#[test]
fn close_all_destroys_every_open_window() {
    let service = PluginWindowService::new();
    let first = service
        .open_window(&WindowOptions::new("one", WindowSize::new(200, 150)))
        .unwrap();
    let second = service
        .open_window(&WindowOptions::new("two", WindowSize::new(200, 150)))
        .unwrap();
    assert_eq!(service.open_count(), 2);
    assert_ne!(first.id(), second.id());

    assert_eq!(service.close_all(), 2);
    assert_eq!(service.open_count(), 0);
    assert!(!is_window(first.hwnd()));
    assert!(!is_window(second.hwnd()));
    assert_eq!(service.close_all(), 0, "a second sweep finds nothing");
}

#[test]
fn dropping_the_service_destroys_its_windows() {
    let service = PluginWindowService::new();
    let window = service
        .open_window(&WindowOptions::new("orphan", WindowSize::new(200, 150)))
        .unwrap();
    let hwnd = window.hwnd();
    assert!(is_window(hwnd));

    drop(service);
    assert!(
        !is_window(hwnd),
        "a dropped service must not leak native windows"
    );
}
