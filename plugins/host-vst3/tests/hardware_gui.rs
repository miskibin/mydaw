//! A real VST3 plugin's editor, in a real MYDAW window, pumped by a real
//! message loop.
//!
//! The VST3 counterpart of `host-clap`'s `fixture_gui`, with one difference that
//! is not a choice: there is no fixture plugin, so this drives whatever is
//! installed and is **opt-in and skip-if-absent** like the rest of
//! `hardware.rs`. A normal test run must never open third-party GUI windows.
//!
//! Nothing here is mocked. The module is loaded through the OS loader, the view
//! comes from the plugin's own `IEditController::createView`, it is attached to a
//! `WS_CHILD` window this host created, and the loop that pumps both is at the
//! bottom of this file -- standing in for tao's, exactly as in
//! `mydaw-plugin-windows`.
//!
//! ```text
//! cargo test -p mydaw-plugin-host-vst3 --test hardware_gui -- --ignored --nocapture --test-threads=1
//! MYDAW_VST3_GUI_HOLD=20 cargo test ...    # leave the editor up, to look at or photograph
//! ```

#![cfg(windows)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use mydaw_plugin_host_vst3::gui::Vst3GuiSession;
use mydaw_plugin_host_vst3::hosting::{LoadedVst3Plugin, Vst3BusLayout};
use mydaw_plugin_host_vst3::test_util::{
    installed_plugins, missing_plugins_message, print_attempts, probe_each,
};
use mydaw_plugin_windows::PluginWindowService;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetWindow, IsWindow, PeekMessageW, TranslateMessage, GW_CHILD, MSG, PM_REMOVE,
};

/// Drain this thread's message queue for `duration`, then return.
///
/// The application's message loop in miniature. A plugin editor that is never
/// pumped never paints, so without this the test would prove only that a window
/// handle exists.
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
    loop {
        // SAFETY: `message` is a live, owned MSG, and this is the thread that
        // owns every window involved.
        unsafe {
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// The plugin's own child window, or `None` if it embedded nothing.
fn child_of(parent: isize) -> Option<isize> {
    // SAFETY: `parent` is a live window on this thread.
    let child = unsafe { GetWindow(parent as HWND, GW_CHILD) };
    (!child.is_null()).then_some(child as isize)
}

fn is_window(handle: isize) -> bool {
    // SAFETY: `IsWindow` is defined for any value, which is the whole point of
    // asking it.
    unsafe { IsWindow(handle as HWND) != 0 }
}

/// (d) A real plugin's editor opens in a MYDAW window, embeds, and closes.
#[test]
#[ignore = "opens an installed plugin GUI; run this hardware test explicitly"]
fn a_real_plugin_editor_opens_embeds_and_closes() {
    if installed_plugins().is_empty() {
        eprintln!("{}", missing_plugins_message());
        return;
    }

    // The window service is thread-affine: it belongs to whichever thread built
    // it, which here is this test's thread, and that is the thread pumping the
    // loop below.
    let windows = Arc::new(PluginWindowService::new());

    // Any plugin with an editor will do; prefer an instrument, because a synth's
    // editor is the one worth photographing.
    let mut candidates = mydaw_plugin_host_vst3::test_util::scanned_instruments();
    candidates.extend(mydaw_plugin_host_vst3::test_util::scanned_effects());

    let service = Arc::clone(&windows);
    let (winner, attempts) = probe_each(
        &candidates,
        |layout: &Vst3BusLayout| layout.is_supported(),
        std::env::var("MYDAW_VST3_GUI").ok().as_deref(),
        move |_info, plugin: &mut LoadedVst3Plugin| open_editor(plugin, &service),
    );
    print_attempts("\n=== VST3 editor probe ===", &attempts);

    let Some((info, mut plugin, mut session)) = winner else {
        eprintln!(
            "SKIPPED: none of the {} plugin(s) tried opened an editor. A plugin with no GUI is \
             an ordinary plugin, not a host failure.",
            attempts.len()
        );
        return;
    };

    let parent = session.hwnd();
    let child = child_of(parent);
    eprintln!("\n=== editor open: '{}' ({}) ===", info.name, info.vendor);
    eprintln!("  host window HWND  : {parent:#x}");
    eprintln!(
        "  plugin child HWND : {}",
        child.map_or_else(|| "(none embedded)".to_owned(), |h| format!("{h:#x}"))
    );
    eprintln!(
        "  negotiated size   : {}x{}",
        session.size().width,
        session.size().height
    );
    eprintln!("  content scale     : {}", session.scale());
    eprintln!("  resizable         : {}", session.can_resize());

    // The assertions that make this evidence rather than decoration.
    assert!(is_window(parent), "the host window must exist");
    assert!(
        session.size().width > 0 && session.size().height > 0,
        "'{}' negotiated a zero-pixel editor",
        info.name
    );
    assert!(
        child.is_some(),
        "'{}' reported a successful attach but embedded no child window",
        info.name
    );
    let child = child.expect("checked");
    assert!(is_window(child));

    // Pump, so the editor actually paints and any resize negotiation it starts
    // is serviced through the same path the shell uses.
    let hold = std::env::var("MYDAW_VST3_GUI_HOLD")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(2);
    if hold > 2 {
        eprintln!("  holding the editor open for {hold}s (MYDAW_VST3_GUI_HOLD)");
    }
    let deadline = Instant::now() + Duration::from_secs(hold);
    while Instant::now() < deadline {
        pump(Duration::from_millis(100));
        let outcome = session.pump(&mut plugin, &windows);
        if let Some(size) = outcome.resized_to {
            eprintln!("  plugin resized to : {}x{}", size.width, size.height);
        }
        if outcome.close_requested {
            eprintln!("  the plugin asked to be closed");
            break;
        }
    }

    // Teardown in the order that matters: detach the view, then the window.
    session.close(&mut plugin, &windows);
    pump(Duration::from_millis(200));

    assert!(
        !is_window(child),
        "'{}' left its child window alive after removed()",
        info.name
    );
    assert_eq!(
        windows.open_count(),
        0,
        "the host window was not closed with the editor"
    );
    assert!(
        plugin.shutdown(),
        "'{}' did not deactivate cleanly after its editor closed",
        info.name
    );
    eprintln!("  editor closed cleanly");
}

/// Open `plugin`'s editor, or say why it has none.
fn open_editor(
    plugin: &mut LoadedVst3Plugin,
    windows: &PluginWindowService,
) -> Result<Vst3GuiSession, String> {
    if !plugin.controller_present() {
        return Err("has no edit controller, so it can have no editor".to_owned());
    }
    Vst3GuiSession::open(plugin, windows, "VST3 editor — MYDAW").map_err(|err| err.to_string())
}
