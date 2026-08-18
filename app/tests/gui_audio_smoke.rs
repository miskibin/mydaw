//! The spike-3 smoke test, minus the webview: a plugin editor open on screen
//! **while audio is playing through that same plugin**.
//!
//! Everything except Tauri is the real thing -- the real audio device, the real
//! CLAP instance in the real RT graph, the real editor window, the real
//! main-thread pump. The one substitution is the event loop: this file runs a
//! `PeekMessage`/`DispatchMessage` loop where the shell runs tao's, and calls
//! [`ClapGuiHost::tick`] where the shell calls it from
//! `RunEvent::MainEventsCleared`.
//!
//! What it proves, in one run:
//!
//! 1. the editor opens through the *application's* path (a queued
//!    [`ClapGuiCommand::Open`], served on the main thread),
//! 2. it animates -- and the animation is driven only by host-serviced CLAP
//!    timers,
//! 3. a resize is negotiated and the plugin really is told its new size,
//! 4. the audio flowing through that plugin is untouched throughout: a capture
//!    taken *while* all of the above happens is still a 440 Hz tone at the
//!    fixture's -6 dB, with no dropped blocks,
//! 5. closing the editor leaves the audio (and the process) alone.
//!
//! `#[ignore]`d: it opens the audio device, makes an audible tone and puts a
//! window on screen.
//!
//! ```sh
//! cargo test -p mydaw-app --test gui_audio_smoke -- --ignored --nocapture
//! ```

#![cfg(windows)]

use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use mydaw_app::gui_bridge::{ClapGuiBridge, ClapGuiCommand};
use mydaw_app::gui_host::ClapGuiHost;
use mydaw_app::{boot_kernel, AppState};
use mydaw_fixture_clap_gain::{GAIN, PLUGIN_ID};
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_host_clap::LoadedClapPlugin;
use mydaw_plugin_test_tone::DEFAULT_AMPLITUDE;
use mydaw_plugin_windows::PluginWindowService;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetWindow, GetWindowTextW, PeekMessageW, SetWindowPos, TranslateMessage,
    GW_CHILD, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
};

/// The test tone at the fixture's gain: what the device should be fed.
const EXPECTED_PEAK: f32 = DEFAULT_AMPLITUDE * GAIN;

/// One pass of the stand-in event loop: drain messages, then let the host do its
/// main-thread work. This is exactly the pair the shell performs.
fn pump_once(host: &mut ClapGuiHost, plugin: &mut LoadedClapPlugin, bridge: &ClapGuiBridge) {
    let mut message = MSG {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
    };
    // SAFETY: `message` is a live, owned MSG.
    unsafe {
        while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    host.tick(Some(plugin), bridge);
}

fn pump_for(
    host: &mut ClapGuiHost,
    plugin: &mut LoadedClapPlugin,
    bridge: &ClapGuiBridge,
    duration: Duration,
) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        pump_once(host, plugin, bridge);
        sleep(Duration::from_millis(4));
    }
}

/// The fixture editor's child window, and the state it publishes as window text.
fn editor_child(parent: isize) -> Option<isize> {
    // SAFETY: `parent` is a live window on this thread.
    let child = unsafe { GetWindow(parent as HWND, GW_CHILD) };
    (!child.is_null()).then_some(child as isize)
}

fn window_text(hwnd: isize) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: `hwnd` is a live window; the buffer is ours.
    let length = unsafe { GetWindowTextW(hwnd as HWND, buffer.as_mut_ptr(), 256) };
    if length <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buffer[..length as usize])
    }
}

fn frame_counter(hwnd: isize) -> Option<u64> {
    window_text(hwnd)
        .split_once("f=")
        .and_then(|(_, tail)| tail.trim().parse().ok())
}

#[test]
#[ignore = "opens the audio device, makes an audible tone and shows a window"]
fn a_plugin_editor_runs_while_its_plugin_processes_audio() {
    // --- boot the shell's world -------------------------------------------
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");
    let windows = kernel
        .ctx()
        .services
        .get::<PluginWindowService>()
        .expect("plugin-windows provides its service");

    let staging = std::env::temp_dir().join(format!("mydaw-gui-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&staging).expect("scratch dir");
    let Some(bundle) = stage_fixture_bundle(&staging) else {
        eprintln!("{}", missing_fixture_message());
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("the fixture loads");

    let info = match state.start_audio_with_effect(Some(&mut plugin)) {
        Ok(info) => info,
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            return;
        }
    };
    eprintln!(
        "device: {} @ {} Hz, {} ch, max block {}",
        info.device_name, info.sample_rate, info.channels, info.max_block
    );
    state.set_playing(true).expect("transport starts");

    let bridge = Arc::clone(state.gui());
    let mut host = ClapGuiHost::new(Arc::clone(&windows));

    // --- open the editor the way the webview does --------------------------
    let opener = {
        let bridge = Arc::clone(&bridge);
        std::thread::spawn(move || bridge.request(ClapGuiCommand::Open))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !host.is_open() && Instant::now() < deadline {
        pump_once(&mut host, &mut plugin, &bridge);
        sleep(Duration::from_millis(4));
    }
    let status = opener
        .join()
        .expect("the opening thread must not panic")
        .expect("the fixture has an editor");
    assert!(status.open, "the editor must report itself open");
    assert_eq!((status.width, status.height), (400, 250));
    assert_eq!(status.plugin_id.as_deref(), Some(PLUGIN_ID));
    assert!(bridge.is_open());

    let hwnd = host.editor_hwnd().expect("an open editor has a window");
    let child = editor_child(hwnd).expect("the plugin embedded its editor");
    eprintln!("editor window {hwnd:#x}, plugin child {child:#x}");

    // --- capture audio *while* the editor is being driven -------------------
    // The capture blocks its thread, so it runs on a worker while this thread
    // keeps doing what the shell's main thread does. That is the point: if the
    // pump and the audio interfered, this is where it would show.
    let tap = Arc::clone(state.debug_tap());
    let capture = std::thread::spawn(move || {
        tap.capture(Duration::from_millis(1200), Duration::from_secs(5))
    });

    let before = frame_counter(child).unwrap_or(0);
    pump_for(&mut host, &mut plugin, &bridge, Duration::from_millis(400));
    let during = frame_counter(child).expect("the editor publishes its frame counter");
    assert!(
        during > before,
        "the editor must animate while audio plays (frames {before} -> {during})"
    );

    // A resize, as a user's drag would produce it. 617x433 is not a multiple of
    // the fixture's size quantum, so only a host that negotiates gets an answer
    // the plugin accepts.
    // SAFETY: `hwnd` is our live editor window on this thread.
    unsafe {
        SetWindowPos(
            hwnd as HWND,
            std::ptr::null_mut(),
            0,
            0,
            617 + 16,
            433 + 39,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    pump_for(&mut host, &mut plugin, &bridge, Duration::from_millis(600));

    let (width, height) = host.editor_size().expect("still open");
    eprintln!("after the resize the plugin reports {width}x{height}");
    assert_eq!(
        width % 10,
        0,
        "the plugin only accepts multiples of 10, so a negotiated width cannot be {width}"
    );
    let painted = window_text(child);
    assert!(
        painted.contains(&format!("{width}x{height}")),
        "the plugin must have been told its new size; it painted {painted:?}"
    );

    let after = frame_counter(child).unwrap();
    assert!(after > during, "the editor must still be animating");

    // --- the audio that ran through all of that ----------------------------
    let capture = capture
        .join()
        .expect("the capture thread must not panic")
        .expect("the tap must produce a capture");
    let stats = capture.stats();
    let left = &stats.per_channel[0];
    let spectrum = mydaw_plugin_debug_tap::spectrum(&capture.channel(0), stats.sample_rate);
    let dominant = spectrum.dominant().expect("a tone has a dominant partial");

    eprintln!(
        "captured {:.2} s, peak {:.4} ({:.2} dBFS), dominant {:.1} Hz, dropped {}",
        stats.duration_seconds,
        left.peak,
        left.peak_dbfs,
        dominant.frequency_hz,
        stats.dropped_samples,
    );
    assert_eq!(
        stats.dropped_samples, 0,
        "the audio thread dropped blocks while the editor was being driven"
    );
    assert!(
        (dominant.frequency_hz - 440.0).abs() < 5.0,
        "the tone must still be 440 Hz, got {:.1} Hz",
        dominant.frequency_hz
    );
    assert!(
        (left.peak - EXPECTED_PEAK).abs() < 0.01,
        "the fixture's -6 dB must still be applied: expected {EXPECTED_PEAK}, got {}",
        left.peak
    );
    assert!(
        !stats.has_non_finite(),
        "NaN or infinity reached the device"
    );

    // --- close, and check the audio survives that too -----------------------
    let closer = {
        let bridge = Arc::clone(&bridge);
        std::thread::spawn(move || bridge.request(ClapGuiCommand::Close))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.is_open() && Instant::now() < deadline {
        pump_once(&mut host, &mut plugin, &bridge);
        sleep(Duration::from_millis(4));
    }
    let closed = closer.join().unwrap().expect("closing must succeed");
    assert!(!closed.open);
    assert!(!host.is_open());
    assert_eq!(windows.open_count(), 0, "the window must be destroyed");
    assert!(
        plugin.timers().is_empty(),
        "the editor's CLAP timer must be unregistered when it closes"
    );

    let tail = state
        .debug_tap()
        .capture(Duration::from_millis(400), Duration::from_secs(5))
        .expect("audio must still be flowing after the editor closed");
    let tail_peak = tail.stats().per_channel[0].peak;
    assert!(
        (tail_peak - EXPECTED_PEAK).abs() < 0.01,
        "closing the editor must not change the audio: peak {tail_peak}"
    );

    // --- shut down in the shell's order -------------------------------------
    state.set_playing(false).ok();
    state.meters().shutdown();
    host.shutdown(Some(&mut plugin));
    state.audio().stop();
    assert!(plugin.shutdown(), "the plugin must deactivate cleanly");
    let _ = std::fs::remove_dir_all(&staging);
}
