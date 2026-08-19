//! The shell's whole CLAP path, minus the webview: a plugin picked, placed,
//! played and edited, with audio running through it the entire time.
//!
//! Everything except Tauri is the real thing -- the real audio device, real CLAP
//! instances in the real RT graph, a real editor window, the real main-thread
//! pump and the real command mailbox. The one substitution is the event loop:
//! this file runs a `PeekMessage`/`DispatchMessage` loop where the shell runs
//! tao's, and calls [`ClapHost::tick`] where the shell calls it from
//! `RunEvent::MainEventsCleared`.
//!
//! What it proves, in one run:
//!
//! 1. a CLAP **instrument** becomes a track's source through the same command a
//!    mixer strip's dropdown sends, and a CLAP **effect** becomes the master
//!    insert through the same command the master strip sends -- two instances,
//!    hosted at once, in one graph;
//! 2. the editor opens for the instance the request *named*, and the request
//!    that names the other one gets that plugin's own answer ("This plugin has
//!    no editor.") rather than the first one's window;
//! 3. the editor animates -- and the animation is driven only by host-serviced
//!    CLAP timers;
//! 4. a resize is negotiated and the plugin really is told its new size;
//! 5. the audio flowing through both plugins is untouched throughout: a capture
//!    taken *while* all of the above happens carries the instrument's note at the
//!    effect's -6 dB, with no dropped blocks;
//! 6. **a track can be added with both plugins loaded** -- the step-5 refusal is
//!    gone -- and both plugins are still audible afterwards;
//! 7. closing the editor and shutting down leaves the audio, and the process,
//!    alone.
//!
//! `#[ignore]`d: it opens the audio device, makes an audible sound and puts a
//! window on screen.
//!
//! ```sh
//! cargo test -p mydaw-app --test gui_audio_smoke -- --ignored --nocapture
//! ```

#![cfg(windows)]

use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use mydaw_app::clap_bridge::{ClapCommand, ClapHostBridge, ClapTarget};
use mydaw_app::clap_host::ClapHost;
use mydaw_app::{boot_kernel, AppState};
use mydaw_fixture_clap_gain::synth::PLUGIN_ID as SYNTH_ID;
use mydaw_fixture_clap_gain::{synth, GAIN, PLUGIN_ID as GAIN_ID};
use mydaw_plugin_audio_io::AudioIoService;
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_windows::PluginWindowService;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetWindow, GetWindowTextW, PeekMessageW, SetWindowPos, TranslateMessage,
    GW_CHILD, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
};

/// The note the instrument holds throughout, and the pitch it should produce.
const KEY: u8 = 69;
const KEY_HZ: f64 = 440.0;

/// One voice of the synth fixture at velocity 1.0, through the gain fixture.
const EXPECTED_PEAK: f32 = synth::VOICE_GAIN * GAIN;

/// One pass of the stand-in event loop: drain messages, then let the host do its
/// main-thread work. This is exactly the pair the shell performs.
fn pump_once(
    host: &mut ClapHost,
    tracks: &mydaw_app::TrackHost,
    audio: &AudioIoService,
    bridge: &ClapHostBridge,
) {
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
    host.tick(tracks, audio, bridge);
}

/// The rig this test drives: everything the shell owns on its main thread.
struct Rig {
    state: AppState,
    host: ClapHost,
    bridge: Arc<ClapHostBridge>,
    tracks: Arc<mydaw_app::TrackHost>,
    audio: Arc<AudioIoService>,
}

impl Rig {
    fn pump(&mut self) {
        pump_once(&mut self.host, &self.tracks, &self.audio, &self.bridge);
    }

    fn pump_for(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.pump();
            sleep(Duration::from_millis(4));
        }
    }

    /// Post `command` from a worker thread -- as a Tauri command would -- while
    /// this thread keeps pumping, and return what the main thread answered.
    fn command<T: Send + 'static>(
        &mut self,
        command: ClapCommand,
        extract: fn(mydaw_app::ClapAnswer) -> T,
    ) -> Result<T, String> {
        let bridge = Arc::clone(&self.bridge);
        let worker = std::thread::spawn(move || bridge.request(command).map(extract));
        let deadline = Instant::now() + Duration::from_secs(6);
        while !worker.is_finished() && Instant::now() < deadline {
            self.pump();
            sleep(Duration::from_millis(2));
        }
        worker.join().expect("the requesting thread must not panic")
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
#[ignore = "opens the audio device, makes an audible sound and shows a window"]
fn two_hosted_plugins_play_and_one_of_them_is_edited_while_they_do() {
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
    let bundle_text = bundle.display().to_string();

    let info = match state.start_audio_with_project() {
        Ok(info) => info,
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            let _ = std::fs::remove_dir_all(&staging);
            return;
        }
    };
    eprintln!(
        "device: {} @ {} Hz, {} ch, max block {}",
        info.device_name, info.sample_rate, info.channels, info.max_block
    );

    let bridge = Arc::clone(state.clap_bridge());
    let tracks = Arc::clone(state.tracks());
    let audio = Arc::clone(state.audio());
    let mut host = ClapHost::new(Arc::clone(&windows));
    host.publish(&tracks, bridge.board());
    bridge.set_served(true);
    let mut rig = Rig {
        state,
        host,
        bridge,
        tracks,
        audio,
    };

    // --- place both plugins, the way the mixer's two pickers do -------------
    rig.command(
        ClapCommand::SetTrackSource {
            track: 0,
            bundle_path: bundle.clone(),
            plugin_id: SYNTH_ID.to_owned(),
        },
        |_| (),
    )
    .expect("the synth fixture must become track 0's source");
    rig.command(
        ClapCommand::SetMasterEffect {
            bundle_path: bundle.clone(),
            plugin_id: GAIN_ID.to_owned(),
        },
        |_| (),
    )
    .expect("the gain fixture must become the master insert");
    assert_eq!(rig.host.instance_count(), 2, "two instances, one graph");

    let mixer = rig.state.mixer_state();
    assert_eq!(
        mixer.tracks[0]
            .clap
            .as_ref()
            .map(|slot| slot.plugin_id.as_str()),
        Some(SYNTH_ID),
        "the mixer must show which plugin heads the track"
    );
    assert_eq!(
        mixer
            .master
            .clap
            .as_ref()
            .map(|slot| slot.plugin_id.as_str()),
        Some(GAIN_ID)
    );
    assert!(
        !mixer.tracks[0].clap.as_ref().unwrap().has_editor,
        "the synth fixture deliberately has no editor, so no button should be offered"
    );
    assert!(mixer.master.clap.as_ref().unwrap().has_editor);

    // Track 1 is the built-in synth and must stay out of the measurement.
    rig.state.set_track_muted(1, true).expect("track 1 exists");
    rig.state.set_selected_track(0).expect("track 0 exists");
    rig.state.set_playing(true).expect("transport starts");
    rig.state
        .note_on(KEY, 1.0)
        .expect("the note reaches track 0");
    rig.pump_for(Duration::from_millis(200));

    // --- the editor opens for the instance the request named ---------------
    let refused = rig.command(ClapCommand::OpenGui(ClapTarget::Track { index: 0 }), |_| ());
    assert_eq!(
        refused.unwrap_err(),
        "This plugin has no editor.",
        "the request must reach the instance it named, and get that plugin's own answer"
    );
    assert_eq!(
        windows.open_count(),
        0,
        "a plugin with no editor must not produce a window"
    );

    let status = rig
        .command(ClapCommand::OpenGui(ClapTarget::Master), |answer| {
            answer.into_gui()
        })
        .expect("the request is answered")
        .expect("an editor status");
    assert!(status.open && status.has_editor);
    assert_eq!((status.width, status.height), (400, 250));
    assert_eq!(status.plugin_id.as_deref(), Some(GAIN_ID));
    assert_eq!(windows.open_count(), 1);

    let master_instance = rig
        .host
        .instance_at(ClapTarget::Master)
        .expect("the master insert holds an instance");
    let hwnd = rig
        .host
        .instances_mut()
        .editor_hwnd(master_instance)
        .expect("an open editor has a window");
    let child = editor_child(hwnd).expect("the plugin embedded its editor");
    eprintln!("editor window {hwnd:#x}, plugin child {child:#x}");

    // --- capture audio *while* the editor is being driven -------------------
    // The capture blocks its thread, so it runs on a worker while this thread
    // keeps doing what the shell's main thread does. That is the point: if the
    // pump and the audio interfered, this is where it would show.
    let tap = Arc::clone(rig.state.debug_tap());
    let capture = std::thread::spawn(move || {
        tap.capture(Duration::from_millis(1200), Duration::from_secs(5))
    });

    let before = frame_counter(child).unwrap_or(0);
    rig.pump_for(Duration::from_millis(400));
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
    rig.pump_for(Duration::from_millis(600));

    let painted = window_text(child);
    eprintln!("after the resize the plugin painted {painted:?}");
    assert!(
        painted.contains('x') && !painted.contains("400x250"),
        "the plugin must have been told a new size; it painted {painted:?}"
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
    let dominant = spectrum
        .dominant()
        .expect("a held note has a dominant partial");

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
        (dominant.frequency_hz - KEY_HZ).abs() < 5.0,
        "the hosted instrument's note must be {KEY_HZ} Hz, got {:.1} Hz",
        dominant.frequency_hz
    );
    assert!(
        (left.peak - EXPECTED_PEAK).abs() < 0.02,
        "the instrument through the effect's -6 dB: expected {EXPECTED_PEAK}, got {}",
        left.peak
    );
    assert!(
        !stats.has_non_finite(),
        "NaN or infinity reached the device"
    );

    // --- the step-5 refusal, deleted ----------------------------------------
    // Adding a track with two hosted plugins in the graph was `ClapInGraph`
    // until step 6. It now rebuilds and re-attaches both instances.
    let tracks_before = rig.state.mixer_state().tracks.len();
    rig.command(
        ClapCommand::AddTrack {
            source_id: "test-tone".to_owned(),
            name: Some("Added".to_owned()),
        },
        |_| (),
    )
    .expect("a layout edit with hosted plugins loaded must now succeed");
    assert_eq!(rig.state.mixer_state().tracks.len(), tracks_before + 1);
    assert_eq!(
        rig.host.instance_count(),
        2,
        "the instances are re-attached, not recreated"
    );
    // The new track is a free-running tone; mute it so the measurement below is
    // still about the hosted plugins.
    rig.state
        .set_track_muted(tracks_before, true)
        .expect("the new track exists");
    rig.state
        .note_on(KEY, 1.0)
        .expect("re-trigger after the rebuild");
    rig.pump_for(Duration::from_millis(400));

    let rebuilt = rig
        .state
        .debug_tap()
        .capture(Duration::from_millis(600), Duration::from_secs(5))
        .expect("audio must still be flowing after the rebuild");
    let rebuilt_peak = rebuilt.stats().per_channel[0].peak;
    eprintln!("after adding a track, peak {rebuilt_peak:.4}");
    assert!(
        (rebuilt_peak - EXPECTED_PEAK).abs() < 0.02,
        "both hosted plugins must be back in the new graph: peak {rebuilt_peak}"
    );
    assert_eq!(
        windows.open_count(),
        1,
        "keeping the instance across a rebuild keeps its editor open"
    );

    // --- close, and check the audio survives that too -----------------------
    let closed = rig
        .command(ClapCommand::CloseGui(ClapTarget::Master), |answer| {
            answer.into_gui()
        })
        .expect("answered")
        .expect("an editor status");
    assert!(!closed.open);
    assert_eq!(windows.open_count(), 0, "the window must be destroyed");

    rig.state.note_off(KEY).ok();
    rig.state.set_playing(false).ok();
    rig.state.meters().shutdown();
    rig.state.audio().stop();
    rig.host.shutdown();
    assert_eq!(rig.host.instance_count(), 0, "every instance destroyed");
    assert_eq!(windows.open_count(), 0);
    let _ = std::fs::remove_dir_all(&staging);
    let _ = bundle_text;
}
