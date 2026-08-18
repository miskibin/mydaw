//! End-to-end plugin GUI: a real CLAP plugin's editor, in a real MYDAW window,
//! pumped by a real message loop.
//!
//! Nothing here is mocked. The fixture plugin is loaded through the OS loader,
//! its `clap.gui` extension is driven through the actual FFI, its editor is a
//! `WS_CHILD` window inside one this host created, and the loop that pumps both
//! is written at the bottom of this file -- standing in for tao's, exactly as in
//! `mydaw-plugin-windows`.
//!
//! The liveness assertion is the point of the whole spike: the fixture's editor
//! advances its frame counter **only** from `clap_plugin_timer_support->on_timer`,
//! and publishes it as its window text. So "the counter went up" means: the host
//! registered the plugin's timer, the host's loop kept turning, the host called
//! `on_timer` on the main thread, and the plugin's window received and processed
//! the repaint. One assertion, the whole chain.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use mydaw_fixture_clap_gain::gui::{PREFERRED_SIZE, SIZE_QUANTUM};
use mydaw_fixture_clap_gain::PLUGIN_ID;
use mydaw_plugin_host_clap::gui::ClapGuiSession;
use mydaw_plugin_host_clap::hosting::LoadedClapPlugin;
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_windows::{PluginWindowService, WindowSize};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetParent, GetWindow, GetWindowTextW, IsWindow, PeekMessageW,
    TranslateMessage, GW_CHILD, MSG, PM_REMOVE,
};

/// Scratch directory that deletes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-clap-gui-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Load the fixture, or `None` (having said why) when it was not built.
fn load_fixture(tag: &str) -> Option<(TempDir, LoadedClapPlugin)> {
    let dir = TempDir::new(tag);
    let bundle = match stage_fixture_bundle(dir.path()) {
        Some(bundle) => bundle,
        None => {
            eprintln!("{}", missing_fixture_message());
            return None;
        }
    };
    let plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("the fixture must load");
    Some((dir, plugin))
}

/// The application's message loop, in miniature: drain the queue, then give the
/// host its main-thread slice (CLAP timers, `on_main_thread`) exactly as the
/// shell does from Tauri's `MainEventsCleared`.
fn pump(plugin: &mut LoadedClapPlugin, duration: Duration) {
    let deadline = Instant::now() + duration;
    let mut message = MSG {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
    };
    loop {
        // SAFETY: `message` is a live, owned MSG.
        unsafe {
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        plugin.tick_main_thread();
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// The plugin's own child window, or `None` if it has not embedded one.
fn child_of(parent: isize) -> Option<isize> {
    // SAFETY: `parent` is a live window on this thread.
    let child = unsafe { GetWindow(parent as HWND, GW_CHILD) };
    (!child.is_null()).then_some(child as isize)
}

/// The window text the fixture's editor publishes ("... f=<frame>").
fn window_text(hwnd: isize) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: `hwnd` is a live window; the buffer is ours and correctly sized.
    let length = unsafe {
        GetWindowTextW(
            hwnd as HWND,
            buffer.as_mut_ptr(),
            i32::try_from(buffer.len()).unwrap(),
        )
    };
    if length <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..length as usize])
}

/// The frame counter the fixture paints, parsed out of its window text.
fn frame_counter(hwnd: isize) -> Option<u64> {
    let text = window_text(hwnd);
    let (_, tail) = text.split_once("f=")?;
    tail.trim().parse().ok()
}

fn is_window(hwnd: isize) -> bool {
    // SAFETY: `IsWindow` accepts any handle value, valid or stale.
    unsafe { IsWindow(hwnd as HWND) != 0 }
}

#[test]
fn the_fixture_editor_opens_animates_resizes_and_closes() {
    let Some((_dir, mut plugin)) = load_fixture("session") else {
        return;
    };
    let windows = PluginWindowService::new();

    let mut session = ClapGuiSession::open(&mut plugin, &windows, "MYDAW Fixture Gain")
        .expect("the fixture advertises an editor");

    // --- the editor is embedded, at the size the plugin asked for ----------
    assert_eq!(session.size(), PREFERRED_SIZE);
    assert!(session.can_resize());
    assert!(is_window(session.hwnd()));
    assert_eq!(
        windows.client_size(session.window_id()).unwrap(),
        WindowSize::new(PREFERRED_SIZE.width, PREFERRED_SIZE.height)
    );

    let child = child_of(session.hwnd()).expect("the plugin must have embedded a child window");
    // SAFETY: both are live windows on this thread.
    assert_eq!(
        unsafe { GetParent(child as HWND) } as isize,
        session.hwnd(),
        "the plugin's window must be a child of the host's"
    );

    // --- liveness: the animation is driven by host timers only -------------
    pump(&mut plugin, Duration::from_millis(250));
    let first = frame_counter(child).expect("the editor publishes its frame counter");
    assert!(
        first > 0,
        "the editor never got a timer tick: the host's timer support or its pump is broken"
    );

    pump(&mut plugin, Duration::from_millis(250));
    let second = frame_counter(child).unwrap();
    assert!(
        second > first,
        "the editor froze after {first} frames (second reading {second})"
    );
    assert!(
        session.pump_ticks() > 0,
        "the host window's wake-up timer never fired"
    );

    // --- resize is negotiated, not imposed ---------------------------------
    // 617x433 is deliberately not a multiple of the fixture's size quantum: a
    // host that skips `adjust_size` would have `set_size` refused and the editor
    // and the window would disagree.
    let dragged = WindowSize::new(617, 433);
    windows
        .set_client_size(session.window_id(), dragged)
        .unwrap();

    let mut negotiated = None;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && negotiated.is_none() {
        pump(&mut plugin, Duration::from_millis(30));
        negotiated = session.pump(&mut plugin, &windows).resized_to;
    }
    let negotiated = negotiated.expect("the resize must be negotiated within two seconds");
    assert_eq!(
        (negotiated.width, negotiated.height),
        (610, 430),
        "the plugin rounds down to multiples of {SIZE_QUANTUM}"
    );
    assert_eq!(session.size(), negotiated);

    // The window was corrected to the size the plugin agreed to, and the editor
    // itself repainted at it.
    pump(&mut plugin, Duration::from_millis(150));
    assert_eq!(
        windows.client_size(session.window_id()).unwrap(),
        WindowSize::new(610, 430)
    );
    let text = window_text(child);
    assert!(
        text.contains("610x430"),
        "the editor should have redrawn at its new size, window text was {text:?}"
    );

    // Nothing else asked to close.
    assert!(!session.pump(&mut plugin, &windows).close_requested);

    // --- teardown ----------------------------------------------------------
    let hwnd = session.hwnd();
    session.close(&mut plugin, &windows);
    assert!(!is_window(hwnd), "the host window must be destroyed");
    assert!(!is_window(child), "the plugin's child window must be gone");
    assert_eq!(windows.open_count(), 0);

    // The plugin survived: it can still be asked for an audio node afterwards.
    assert!(plugin.create_node(48_000.0, 256).is_ok());
    pump(&mut plugin, Duration::from_millis(20));
}

#[test]
fn an_editor_can_be_opened_and_closed_repeatedly() {
    let Some((_dir, mut plugin)) = load_fixture("reopen") else {
        return;
    };
    let windows = PluginWindowService::new();

    for round in 0..3 {
        let mut session = ClapGuiSession::open(&mut plugin, &windows, "MYDAW Fixture Gain")
            .unwrap_or_else(|err| panic!("round {round} failed to open the editor: {err}"));
        assert_eq!(session.size(), PREFERRED_SIZE, "round {round}");
        pump(&mut plugin, Duration::from_millis(120));

        let child = child_of(session.hwnd()).expect("a child window per round");
        assert!(
            frame_counter(child).unwrap_or(0) > 0,
            "round {round}: the editor must animate every time it is opened"
        );

        assert!(!session.pump(&mut plugin, &windows).close_requested);
        session.close(&mut plugin, &windows);
        assert_eq!(windows.open_count(), 0, "round {round}");
    }

    // Timers registered by each session were unregistered when it closed, so
    // nothing accumulates across rounds.
    assert!(
        plugin.timers().is_empty(),
        "a closed editor must not leave timers behind"
    );
}

#[test]
fn closing_the_window_is_reported_to_the_owner() {
    let Some((_dir, mut plugin)) = load_fixture("close") else {
        return;
    };
    let windows = PluginWindowService::new();
    let mut session =
        ClapGuiSession::open(&mut plugin, &windows, "MYDAW Fixture Gain").expect("editor opens");

    // SAFETY: posting to a live window on this thread.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
            session.hwnd() as HWND,
            windows_sys::Win32::UI::WindowsAndMessaging::WM_CLOSE,
            0,
            0,
        )
    };

    let mut requested = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !requested {
        pump(&mut plugin, Duration::from_millis(30));
        requested = session.pump(&mut plugin, &windows).close_requested;
    }
    assert!(requested, "the close button must reach the session's owner");

    let hwnd = session.hwnd();
    session.close(&mut plugin, &windows);
    assert!(!is_window(hwnd));
}

#[test]
fn an_editor_that_is_never_opened_costs_nothing() {
    let Some((_dir, mut plugin)) = load_fixture("idle") else {
        return;
    };
    // No GUI, no timers: ticking the main thread is a no-op the shell can call
    // 60 times a second forever.
    let work = plugin.tick_main_thread();
    assert!(work.is_idle(), "an idle plugin must do no main-thread work");
    assert!(plugin.timers().is_empty());
    assert!(plugin.gui_requests().take_resize().is_none());
}
