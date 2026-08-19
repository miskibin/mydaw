//! The `#[tauri::command]` surface. Contract: `ui/README.md` + `ui/src/api/backend.ts`.
//!
//! Every command is a one-liner over [`AppState`]: the shell keeps its logic in
//! testable modules and this file only adapts it to Tauri's calling convention
//! (managed state in, `Result<_, String>` out -- `AudioIoError` is not
//! `Serialize`, and the UI shows the message as-is in the status bar).

use mydaw_ipc_bridge::UiSnapshot;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::debug_capture::{capture_root, capture_to_disk, CaptureReportDto};
use crate::dto::{ClapPluginInfoDto, MeterFrameDto, PatternDto, SequencerStateDto};
use crate::gui_bridge::{ClapGuiCommand, ClapGuiStatusDto};
use crate::ipc_bench::{
    self, BenchPayload, BenchReport, BenchRunInput, EpochSample, EpochSync, StreamConfig,
};
use crate::project::MixerStateDto;
use crate::source::{Source, SourceDto};
use crate::state::AppState;

/// Plugins and panels, as of boot. Called once by the UI on startup.
#[tauri::command]
pub fn get_ui_snapshot(state: State<'_, AppState>) -> UiSnapshot {
    state.snapshot()
}

/// Installed CLAP plugins, for the browser panel.
///
/// The first call scans the standard plugin directories, which loads every
/// installed bundle into this process; later calls serve the cache. Pass
/// `rescan: true` (the panel's Rescan button) to walk the directories again --
/// that is how a plugin installed while MYDAW was running becomes visible.
///
/// Marked `async` so Tauri runs it off the main thread: on a machine with a large
/// plugin collection a scan takes long enough to stutter the window.
#[tauri::command]
pub async fn get_clap_plugins(
    state: State<'_, AppState>,
    rescan: bool,
) -> Result<Vec<ClapPluginInfoDto>, String> {
    Ok(state.clap_plugins(rescan))
}

/// Start the transport. The position is wherever it was left.
#[tauri::command]
pub fn transport_play(state: State<'_, AppState>) -> Result<(), String> {
    state.set_playing(true).map_err(|err| err.to_string())
}

/// Stop the transport, keeping the playhead where it is (no implicit rewind --
/// the UI decides what Stop means visually).
#[tauri::command]
pub fn transport_stop(state: State<'_, AppState>) -> Result<(), String> {
    state.set_playing(false).map_err(|err| err.to_string())
}

/// Play a note on the loaded instrument.
///
/// `key` is a MIDI key number (60 = middle C, 69 = A440) and `velocity` is
/// `0.0..=1.0`. Sounds whether or not the transport is running: this is live
/// input, not sequenced playback.
///
/// Synchronous on purpose. The whole command is a bounds check and one wait-free
/// queue push, so hopping to a worker pool would cost more than it does, and a
/// held chord is a burst of these -- latency here is audible.
#[tauri::command]
pub fn note_on(state: State<'_, AppState>, key: u8, velocity: f32) -> Result<(), String> {
    state.note_on(key, velocity).map_err(|err| err.to_string())
}

/// Release a note. Not an error if that key was never pressed -- the plugin
/// decides what an unmatched note-off means.
#[tauri::command]
pub fn note_off(state: State<'_, AppState>, key: u8) -> Result<(), String> {
    state.note_off(key).map_err(|err| err.to_string())
}

/// Every source the session can play, marked with which one the **selected
/// track** is playing and which are available. Called once at startup and again
/// after every switch.
#[tauri::command]
pub fn get_sources(state: State<'_, AppState>) -> Vec<SourceDto> {
    state.sources()
}

/// Put a different instrument at the head of the selected track.
///
/// `source` is one of the ids from [`get_sources`]. The switch is a node swap
/// on the running graph, so the mixer, the other tracks and a CLAP master insert
/// all keep playing through it (see `crate::tracks`).
///
/// `async` because `clap` -- and only `clap` -- has to wait for the main thread
/// to activate the plugin; the built-in instruments answer immediately, from
/// this thread. Running the whole command on the blocking pool means the one
/// slow case cannot stall the window that has to serve it.
#[tauri::command]
pub async fn set_source(
    state: State<'_, AppState>,
    source: String,
) -> Result<Vec<SourceDto>, String> {
    state
        .set_source_id(&source)
        .map_err(|err| err.to_string())?;
    Ok(state.sources())
}

// ---------------------------------------------------------------------------
// Mixer
// ---------------------------------------------------------------------------
//
// Every one of these returns the whole mixer state rather than an
// acknowledgement. A strip is a handful of numbers, the IPC spike measured a
// message this size as free, and one shape back means the panel never has to
// merge a partial update into what it already had -- which is where optimistic
// UIs usually go wrong.

/// The project's tracks, the master strip and the current selection.
#[tauri::command]
pub fn get_tracks(state: State<'_, AppState>) -> MixerStateDto {
    state.mixer_state()
}

/// Set a track's fader, in dB (`-60..=6`).
///
/// Succeeds with no audio device open -- a fader is project data. The change is
/// an atomic store the audio thread picks up on its next block and ramps over
/// 10 ms, so dragging a fader costs the same as clicking one.
#[tauri::command]
pub fn set_track_gain(
    state: State<'_, AppState>,
    track: usize,
    db: f32,
) -> Result<MixerStateDto, String> {
    state
        .set_track_gain_db(track, db)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Set a track's pan, `-1.0` hard left to `1.0` hard right.
#[tauri::command]
pub fn set_track_pan(
    state: State<'_, AppState>,
    track: usize,
    pan: f32,
) -> Result<MixerStateDto, String> {
    state
        .set_track_pan(track, pan)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Mute or unmute a track. A mute is a 10 ms fade, not a cut.
#[tauri::command]
pub fn set_track_mute(
    state: State<'_, AppState>,
    track: usize,
    muted: bool,
) -> Result<MixerStateDto, String> {
    state
        .set_track_muted(track, muted)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Solo or un-solo a track.
///
/// Solo is resolved here, not on the audio thread: with anything soloed, every
/// track that is not soloed gets an effective mute. See `crate::project`.
#[tauri::command]
pub fn set_track_solo(
    state: State<'_, AppState>,
    track: usize,
    soloed: bool,
) -> Result<MixerStateDto, String> {
    state
        .set_track_soloed(track, soloed)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Set the master fader, in dB.
#[tauri::command]
pub fn set_master_gain(state: State<'_, AppState>, db: f32) -> Result<MixerStateDto, String> {
    state
        .set_master_gain_db(db)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Choose the track the computer-keyboard piano plays and the source selector
/// addresses.
#[tauri::command]
pub fn set_selected_track(
    state: State<'_, AppState>,
    track: usize,
) -> Result<MixerStateDto, String> {
    state
        .set_selected_track(track)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Put a different instrument at the head of one track, by index.
///
/// The mixer's own source picker. `set_source` is the same operation aimed at
/// whichever track is selected.
///
/// `async` for the same reason as [`set_source`]: `clap` waits for the main
/// thread.
#[tauri::command]
pub async fn set_track_source(
    state: State<'_, AppState>,
    track: usize,
    source: String,
) -> Result<MixerStateDto, String> {
    let source = Source::parse(&source).map_err(|err| err.to_string())?;
    state
        .set_track_source(track, source)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Add a track and rebuild the graph.
///
/// Refused while a hosted CLAP plugin is in the graph -- a rebuild recreates
/// every node, and a CLAP plugin can only be activated once. See
/// `TrackError::ClapInGraph`.
#[tauri::command]
pub fn add_track(
    state: State<'_, AppState>,
    source: String,
    name: Option<String>,
) -> Result<MixerStateDto, String> {
    let source = Source::parse(&source).map_err(|err| err.to_string())?;
    state
        .add_track(source, name)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Remove a track and rebuild the graph.
///
/// Refused for the last track, and while a CLAP plugin is in the graph. Note
/// that removing a track renumbers the ones after it, and a track index *is* a
/// note channel -- so a sequencer row addressed at a later track follows the
/// renumbering.
#[tauri::command]
pub fn remove_track(state: State<'_, AppState>, track: usize) -> Result<MixerStateDto, String> {
    state.remove_track(track).map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// The step grid and the session tempo. Called once at startup.
///
/// Both in one message because the panel needs both to draw itself: the grid
/// needs the pattern, and the moving current-step highlight needs the tempo. See
/// [`SequencerStateDto`] for why they do not ride in `get_ui_snapshot`.
#[tauri::command]
pub fn get_pattern(state: State<'_, AppState>) -> SequencerStateDto {
    state.sequencer_state()
}

/// Replace the step grid, returning the state the session now holds.
///
/// A whole-pattern replacement rather than a per-step edit: a 4x16 grid is about
/// a kilobyte of JSON, the IPC spike measured that as free, and one message per
/// edit is a protocol with no ordering problems, no missed updates and no diff
/// to get wrong.
///
/// Succeeds with no audio device open -- a pattern is project data, not stream
/// state (see [`AppState::set_pattern`]). Rejects a grid the engine could not
/// run, with a sentence naming the row that did it.
#[tauri::command]
pub fn set_pattern(
    state: State<'_, AppState>,
    pattern: PatternDto,
) -> Result<SequencerStateDto, String> {
    state
        .set_pattern_dto(&pattern)
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Set the session tempo, returning the value that was stored.
///
/// Out-of-range values are rejected rather than clamped, so a user who typed
/// 5000 is told rather than silently given 999.
#[tauri::command]
pub fn set_bpm(state: State<'_, AppState>, bpm: f64) -> Result<f64, String> {
    state.set_bpm(bpm).map_err(|err| err.to_string())
}

/// Capture `seconds` of the engine's output and write a WAV, an annotated PNG
/// and a statistics JSON under `.debug/captures/<timestamp>/`.
///
/// Blocks for roughly `seconds` while the audio thread fills the tap, so it
/// runs on Tauri's blocking pool (`async` + a blocking body) rather than on the
/// main thread: a two-second capture must not freeze the window.
///
/// Errors are strings because the UI shows them in the status bar as-is; the
/// typed variants live in [`crate::debug_capture::DebugCaptureError`].
#[tauri::command]
pub async fn debug_capture(
    state: State<'_, AppState>,
    seconds: f32,
) -> Result<CaptureReportDto, String> {
    let outcome = capture_to_disk(state.debug_tap(), seconds, &capture_root())
        .map_err(|err| err.to_string())?;
    Ok(CaptureReportDto::from(&outcome))
}

/// Open the loaded CLAP effect's editor in its own native window.
///
/// The work happens on the main thread -- a CLAP editor is `[main-thread]` and
/// its window belongs to the event loop's thread -- so this posts a request and
/// waits for the answer (see [`crate::gui_bridge`]). `async` so the wait happens
/// on Tauri's blocking pool rather than on the main thread, which is the one that
/// has to answer it.
///
/// Errors are the user-facing sentence, e.g. "This plugin has no editor." -- a
/// plugin without a GUI is normal, not a malfunction.
#[tauri::command]
pub async fn open_clap_gui(state: State<'_, AppState>) -> Result<ClapGuiStatusDto, String> {
    state.gui().request(ClapGuiCommand::Open)
}

/// Close the plugin editor. Not an error when nothing is open.
#[tauri::command]
pub async fn close_clap_gui(state: State<'_, AppState>) -> Result<ClapGuiStatusDto, String> {
    state.gui().request(ClapGuiCommand::Close)
}

/// Stream throttled meter frames into `channel` until the webview drops it.
///
/// One subscription is live at a time; a second call (a page reload) replaces the
/// first. `Channel::send` failing is how the pump learns the receiver is gone.
#[tauri::command]
pub fn subscribe_meters(state: State<'_, AppState>, channel: Channel<MeterFrameDto>) {
    state
        .meters()
        .subscribe(Box::new(move |frame| channel.send(frame).is_ok()));
}

// ---------------------------------------------------------------------------
// IPC benchmark (spike 4)
// ---------------------------------------------------------------------------
//
// Dev-only surface. These commands are always registered -- the invoke handler
// is a compile-time match, so an unused arm costs nothing -- but nothing calls
// them unless `MYDAW_DEV_PANELS`/`MYDAW_IPC_BENCH` put the bench panel in the
// snapshot. See `crate::ipc_bench`.

/// The bench clock, right now. The webview's half of the epoch handshake, and
/// the thing whose round trip *is* the RTT measurement.
///
/// Deliberately synchronous: Tauri runs a sync command on the main thread, which
/// is the same thread a real UI command competes for, so this is the honest
/// number for "what does an `invoke` cost in this app".
#[tauri::command]
pub fn bench_ping() -> f64 {
    ipc_bench::epoch_ms()
}

/// The same ping, on Tauri's worker pool, so the report can show what `async`
/// buys (or costs) against the main-thread path.
#[tauri::command]
pub async fn bench_ping_async() -> f64 {
    ipc_bench::epoch_ms()
}

/// The scenario matrix this run should execute. Defined in Rust so the UI, the
/// auto run and the tests all read the same list; `MYDAW_IPC_BENCH=stress`
/// appends the past-the-cliff scenarios.
#[tauri::command]
pub fn bench_plan() -> Vec<StreamConfig> {
    ipc_bench::env_plan()
}

/// Reconcile the two clocks from the webview's ping handshake.
///
/// The UI could do this arithmetic itself; it deliberately does not. One
/// implementation, in the language that has the tests, is what keeps the offset
/// used *during* a run identical to the offset printed in the report.
///
/// `None` when the handshake contained nothing usable.
#[tauri::command]
pub fn bench_sync(samples: Vec<EpochSample>) -> Option<EpochSync> {
    ipc_bench::estimate_epoch_offset(&samples)
}

/// Stream one scenario's frames into `channel`, returning how many were
/// scheduled so the caller knows when it has them all.
///
/// The frames go out from a dedicated thread: this command must return
/// immediately (it runs on the main thread) and the pacing must not be at the
/// mercy of Tauri's executor. `InvokeResponseBody` is the channel payload
/// because it is the one type that carries *either* a pre-serialised JSON string
/// or raw bytes -- the two transports under test -- with no further conversion.
///
/// # Errors
/// A configuration outside the harness's limits, or a thread that will not spawn.
#[tauri::command]
pub fn bench_stream(
    config: StreamConfig,
    channel: Channel<InvokeResponseBody>,
) -> Result<u32, String> {
    config.validate().map_err(|err| err.to_string())?;
    let frames = config.frame_count();
    std::thread::Builder::new()
        .name("mydaw-ipc-bench".to_owned())
        .spawn(move || {
            let sent = ipc_bench::run_stream(config, |payload| {
                let body = match payload {
                    BenchPayload::Json(text) => InvokeResponseBody::Json(text),
                    BenchPayload::Binary(bytes) => InvokeResponseBody::Raw(bytes),
                };
                channel.send(body).is_ok()
            });
            if sent < frames {
                log::warn!("ipc bench: the webview stopped receiving after {sent}/{frames} frames");
            }
        })
        .map_err(|err| format!("could not start the bench sender: {err}"))?;
    Ok(frames)
}

/// Turn the live meter path's bench timestamp on or off.
///
/// This is how the *real* meter latency is measured rather than modelled: the
/// pump stamps the frame it is about to send, the webview subtracts.
#[tauri::command]
pub fn bench_meter_stamp(state: State<'_, AppState>, enabled: bool) {
    state.meters().set_stamping(enabled);
}

/// Aggregate a finished run, write it to `.debug/ipc-bench/<run id>.json`, print
/// the report to stdout, and -- in auto mode only -- exit the app.
///
/// The exit is deferred by a beat so this command's own response reaches the
/// webview first; an agent reads the stdout report, so a truncated response
/// would cost nothing, but a truncated *report* would cost the run.
///
/// # Errors
/// A results file that cannot be written.
#[tauri::command]
pub async fn bench_finish(
    app: tauri::AppHandle,
    run: BenchRunInput,
) -> Result<BenchReport, String> {
    let report = ipc_bench::build_report(&run, ipc_bench::run_id(std::time::SystemTime::now()));
    let path = ipc_bench::write_report(&report, &capture_root())
        .map_err(|err| format!("could not write the bench results: {err}"))?;
    print!("{}", ipc_bench::report_text(&report, Some(&path)));

    if ipc_bench::auto_mode() {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            app.exit(0);
        });
    }
    Ok(report)
}
