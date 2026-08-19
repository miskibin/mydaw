//! The `#[tauri::command]` surface. Contract: `ui/README.md` + `ui/src/api/backend.ts`.
//!
//! Every command is a one-liner over [`AppState`]: the shell keeps its logic in
//! testable modules and this file only adapts it to Tauri's calling convention
//! (managed state in, `Result<_, String>` out -- `AudioIoError` is not
//! `Serialize`, and the UI shows the message as-is in the status bar).

use std::path::PathBuf;

use mydaw_ipc_bridge::UiSnapshot;
use mydaw_plugin_sampler::AssetStatus;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::clap_bridge::{ClapGuiStatusDto, ClapTarget};
use crate::debug_capture::{capture_root, capture_to_disk, CaptureReportDto};
use crate::dto::{
    ClapPluginInfoDto, MeterFrameDto, PatternDto, SequencerStateDto, StreamInfoDto,
    Vst3PluginInfoDto,
};
use crate::ipc_bench::{
    self, BenchPayload, BenchReport, BenchRunInput, EpochSample, EpochSync, StreamConfig,
};
use crate::project::MixerStateDto;
use crate::project_bridge::ProjectInfoDto;
use crate::project_file::{PROJECT_EXTENSION, UNTITLED};
use crate::source::{Source, SourceDto};
use crate::state::AppState;
use crate::vst3_bridge::Vst3GuiStatusDto;
use mydaw_plugin_sequencer::{PlayMode, DEFAULT_CLIP_TICKS};

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

/// Whether the bundled sample content is installed, and what the downloader is
/// doing about it.
///
/// Polled by the shell while a download runs, which is what draws the
/// percentage. Cheap: file metadata plus three atomic loads, never a hash.
#[tauri::command]
pub fn get_asset_status(state: State<'_, AppState>) -> AssetStatus {
    state.asset_status()
}

/// Download the bundled sample content.
///
/// Returns as soon as the transfer has *started*; the shell polls
/// [`get_asset_status`] for progress. When it finishes, the sampler rescans and
/// the new instrument appears in [`get_sources`] with no restart.
///
/// Nothing calls this except a user pressing the button. See the consent note
/// in `mydaw_plugin_sampler::download`.
#[tauri::command]
pub fn start_asset_download(state: State<'_, AppState>) -> Result<AssetStatus, String> {
    state.start_asset_download()?;
    Ok(state.asset_status())
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

/// Put a different **built-in** instrument at the head of one track, by index.
///
/// The mixer's own source picker. `set_source` is the same operation aimed at
/// whichever track is selected. A hosted CLAP instrument goes through
/// [`set_track_source_clap`], which carries the bundle path too.
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

/// Put a hosted CLAP instrument at the head of one track.
///
/// `bundlePath` and `pluginId` come straight from an entry in [`get_sources`]
/// or [`get_clap_plugins`]. Both travel because two copies of the same plugin
/// can be installed and an id alone would not say which was meant.
///
/// The instance is created, activated and swapped in **on the main thread**
/// (a CLAP instance is `[main-thread]`-only), so this waits for it -- hence
/// `async`, which keeps the wait on Tauri's blocking pool rather than on the
/// thread that has to answer it.
#[tauri::command]
pub async fn set_track_source_clap(
    state: State<'_, AppState>,
    track: usize,
    bundle_path: String,
    plugin_id: String,
) -> Result<MixerStateDto, String> {
    state
        .set_track_source_clap(track, &bundle_path, &plugin_id)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Scanned CLAP **audio effects**, for the master insert picker.
///
/// The counterpart of [`get_sources`], which lists instruments: an effect cannot
/// head a track and an instrument cannot process the master bus, so the two
/// pickers are two lists rather than one with a warning.
#[tauri::command]
pub async fn get_master_effects(
    state: State<'_, AppState>,
) -> Result<Vec<ClapPluginInfoDto>, String> {
    Ok(state.master_effects())
}

/// Put a hosted CLAP effect in the master insert, replacing any other.
#[tauri::command]
pub async fn set_master_effect_clap(
    state: State<'_, AppState>,
    bundle_path: String,
    plugin_id: String,
) -> Result<MixerStateDto, String> {
    state
        .set_master_effect_clap(&bundle_path, &plugin_id)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Empty the master insert and destroy the plugin that was in it.
#[tauri::command]
pub async fn clear_master_effect(state: State<'_, AppState>) -> Result<MixerStateDto, String> {
    state.clear_master_effect().map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Add a track and rebuild the graph.
///
/// Since step 6 this is allowed with hosted CLAP plugins loaded: the rebuild
/// installs a plugin-free graph and the main thread re-attaches every instance
/// into it, at the cost of a few milliseconds of silence from those plugins
/// only. `async` because that path waits for the main thread.
#[tauri::command]
pub async fn add_track(
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
/// Refused for the last track. Note that removing a track renumbers the ones
/// after it, and a track index *is* a note channel -- so a sequencer row
/// addressed at a later track follows the renumbering.
#[tauri::command]
pub async fn remove_track(
    state: State<'_, AppState>,
    track: usize,
) -> Result<MixerStateDto, String> {
    state.remove_track(track).map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

// ---------------------------------------------------------------------------
// Patterns, the arrangement and the play mode
// ---------------------------------------------------------------------------
//
// Every one of these returns the **whole** [`SequencerStateDto`] rather than an
// acknowledgement, for the same reason the mixer commands return the whole
// mixer: one shape back means a panel never merges a partial update into what
// it already had, which is where optimistic UIs go wrong. The IPC spike
// measured a message this size as free.
//
// Every one of them also succeeds with **no audio device open**. Patterns and
// arrangements are project data; a user on a machine with no sound card must
// still be able to write a beat, and the session republishes itself when a
// stream comes up.

/// Add an empty pattern with a grid on every rack channel, returning the new
/// state.
///
/// The new pattern is **not** made active: adding one and switching to it are
/// two gestures, and a command that did both could not express the other. Send
/// [`set_active_pattern`] after it if that is what the button means.
#[tauri::command]
pub fn add_pattern(
    state: State<'_, AppState>,
    name: Option<String>,
) -> Result<SequencerStateDto, String> {
    state.add_pattern(name).map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Duplicate a pattern, content and all. The copy lands directly after it.
#[tauri::command]
pub fn clone_pattern(
    state: State<'_, AppState>,
    pattern_id: u64,
    name: Option<String>,
) -> Result<SequencerStateDto, String> {
    state
        .edit_patterns(|set| set.clone_pattern(pattern_id, name))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Delete a pattern **and every playlist clip that played it**.
///
/// The two are one command because they must not be separable: a set without
/// the pattern and an arrangement still naming it is a state no caller should
/// be able to publish. Refused for the last pattern.
#[tauri::command]
pub fn remove_pattern(
    state: State<'_, AppState>,
    pattern_id: u64,
) -> Result<SequencerStateDto, String> {
    state
        .remove_pattern(pattern_id)
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Rename a pattern. A blank name is refused -- the picker would show an empty
/// row.
#[tauri::command]
pub fn rename_pattern(
    state: State<'_, AppState>,
    pattern_id: u64,
    name: String,
) -> Result<SequencerStateDto, String> {
    state
        .edit_patterns(|set| set.rename(pattern_id, name))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Set a pattern's accent colour, or clear it with `null`.
///
/// The engine only stores it: there is no palette here, and inventing one would
/// put a design decision in a file format.
#[tauri::command]
pub fn set_pattern_color(
    state: State<'_, AppState>,
    pattern_id: u64,
    color: Option<String>,
) -> Result<SequencerStateDto, String> {
    state
        .edit_patterns(|set| set.set_color(pattern_id, color))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Set a pattern's loop length, in steps: 16, 32 or 64.
///
/// Shortening is **not** destructive -- cells and notes past the new end are
/// kept and simply not played, so lengthening again gives them back. The one
/// exception is a note that would then hang past the bar line, which is refused.
#[tauri::command]
pub fn set_pattern_length(
    state: State<'_, AppState>,
    pattern_id: u64,
    steps: u8,
) -> Result<SequencerStateDto, String> {
    state
        .edit_patterns(|set| set.set_length(pattern_id, steps))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Choose which pattern the grid, the roll and PAT mode address.
#[tauri::command]
pub fn set_active_pattern(
    state: State<'_, AppState>,
    pattern_id: u64,
) -> Result<SequencerStateDto, String> {
    state
        .edit_patterns(|set| set.set_active(pattern_id))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// The whole sequencer: patterns, the arrangement, the tempo, the mode and the
/// tick geometry. Called once at startup.
#[tauri::command]
pub fn get_pattern(state: State<'_, AppState>) -> SequencerStateDto {
    state.sequencer_state()
}

/// Replace one pattern wholesale.
///
/// A whole-pattern replacement rather than a per-step edit: a rack of grids is
/// a few kilobytes of JSON, the IPC spike measured that as free, and one message
/// per edit is a protocol with no ordering problems, no missed updates and no
/// diff to get wrong.
///
/// `pattern.id` names which pattern; `0` (or absent) means the active one.
/// Rejects a pattern the engine could not run, with a sentence naming what did
/// it.
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

/// Switch the transport between `pat` and `song`.
///
/// Also accepts `pattern` for `pat`. Takes effect on the audio thread's next
/// block, at the same latency as pressing play.
#[tauri::command]
pub fn set_play_mode(
    state: State<'_, AppState>,
    mode: String,
) -> Result<SequencerStateDto, String> {
    let mode = PlayMode::parse(&mode)
        .ok_or_else(|| format!("'{mode}' is not a play mode; expected 'pat' or 'song'"))?;
    state.set_play_mode(mode).map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

// --------------------------------------------------------------- the playlist

/// Append an arrangement lane.
#[tauri::command]
pub fn add_playlist_track(
    state: State<'_, AppState>,
    name: Option<String>,
) -> Result<SequencerStateDto, String> {
    state
        .edit_playlist(|list| list.add_track(name))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Remove a lane **and every clip on it**, shifting the lanes below up.
///
/// The clips go because a clip's lane is its position rather than a reference:
/// one left behind would have to move somewhere, and every somewhere is a silent
/// rewrite of the arrangement. Refused for the last lane.
#[tauri::command]
pub fn remove_playlist_track(
    state: State<'_, AppState>,
    track: usize,
) -> Result<SequencerStateDto, String> {
    state
        .edit_playlist(|list| list.remove_track(track).map(|_| ()))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Rename a lane.
#[tauri::command]
pub fn rename_playlist_track(
    state: State<'_, AppState>,
    track: usize,
    name: String,
) -> Result<SequencerStateDto, String> {
    state
        .edit_playlist(|list| list.rename_track(track, name))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Mute or unmute a lane.
///
/// Silences it; it does not shorten the song, so the playhead still runs to the
/// end of a muted lane's last clip.
#[tauri::command]
pub fn set_playlist_track_mute(
    state: State<'_, AppState>,
    track: usize,
    muted: bool,
) -> Result<SequencerStateDto, String> {
    state
        .edit_playlist(|list| list.set_track_muted(track, muted))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Place a pattern on the timeline.
///
/// `start` and `length` are in **ticks** (960 to the quarter note, 3840 to the
/// bar). A length longer than the pattern loops it; shorter trims it. `length`
/// absent means four bars.
#[tauri::command]
pub fn add_pattern_clip(
    state: State<'_, AppState>,
    pattern_id: u64,
    track: usize,
    start: u64,
    length: Option<u64>,
) -> Result<SequencerStateDto, String> {
    let length = length.unwrap_or(DEFAULT_CLIP_TICKS);
    state
        .edit_playlist(|list| list.add_pattern_clip(pattern_id, track, start, length))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Remove a clip.
#[tauri::command]
pub fn remove_clip(state: State<'_, AppState>, clip_id: u64) -> Result<SequencerStateDto, String> {
    state
        .edit_playlist(|list| list.remove_clip(clip_id).map(|_| ()))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Move a clip to another lane and start tick.
#[tauri::command]
pub fn move_clip(
    state: State<'_, AppState>,
    clip_id: u64,
    track: usize,
    start: u64,
) -> Result<SequencerStateDto, String> {
    state
        .edit_playlist(|list| list.move_clip(clip_id, track, start))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Change a clip's length, in ticks. Zero is refused.
#[tauri::command]
pub fn resize_clip(
    state: State<'_, AppState>,
    clip_id: u64,
    length: u64,
) -> Result<SequencerStateDto, String> {
    state
        .edit_playlist(|list| list.resize_clip(clip_id, length))
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

/// What the open audio device is: the status bar's BUFFER and LATENCY.
///
/// Answers with a `running: false` shape rather than an error when no stream is
/// open, because "no device yet" is an ordinary state a status bar has to draw
/// and not something to put in the error line.
#[tauri::command]
pub fn get_stream_info(state: State<'_, AppState>) -> StreamInfoDto {
    state
        .audio()
        .stream_info()
        .map_or_else(StreamInfoDto::stopped, StreamInfoDto::from)
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

/// Open one hosted plugin's editor in its own native window.
///
/// `target` is `{"slot":"track","index":n}` or `{"slot":"master"}`. **Several
/// editors can be open at once**, one per placed instance.
///
/// The work happens on the main thread -- a CLAP editor is `[main-thread]` and
/// its window belongs to the event loop's thread -- so this posts a request and
/// waits for the answer (see [`crate::clap_bridge`]). `async` so the wait happens
/// on Tauri's blocking pool rather than on the main thread, which is the one that
/// has to answer it.
///
/// Errors are the user-facing sentence, e.g. "This plugin has no editor." -- a
/// plugin without a GUI is normal, not a malfunction.
#[tauri::command]
pub async fn open_clap_gui(
    state: State<'_, AppState>,
    target: ClapTarget,
) -> Result<ClapGuiStatusDto, String> {
    state.open_clap_gui(target).map_err(|err| err.to_string())
}

/// Close one plugin's editor. Not an error when nothing is open.
#[tauri::command]
pub async fn close_clap_gui(
    state: State<'_, AppState>,
    target: ClapTarget,
) -> Result<ClapGuiStatusDto, String> {
    state.close_clap_gui(target).map_err(|err| err.to_string())
}

/// Serialise a hosted plugin's state, as base64.
///
/// Groundwork for the project file (step 8): this is exactly the call a save
/// will make per instance. An **empty** string means the plugin implements no
/// `state` extension and has nothing to persist, which is a normal answer.
///
/// Base64 rather than a byte array because the blob is opaque binary and a JSON
/// array of numbers would be four characters per byte.
#[tauri::command]
pub async fn save_clap_state(
    state: State<'_, AppState>,
    target: ClapTarget,
) -> Result<String, String> {
    let bytes = state
        .save_clap_state(target)
        .map_err(|err| err.to_string())?;
    Ok(crate::clap_bridge::to_base64(&bytes))
}

/// Restore a hosted plugin from a blob produced by [`save_clap_state`].
#[tauri::command]
pub async fn load_clap_state(
    state: State<'_, AppState>,
    target: ClapTarget,
    state_base64: String,
) -> Result<(), String> {
    let bytes = crate::clap_bridge::from_base64(&state_base64)?;
    state
        .load_clap_state(target, bytes)
        .map_err(|err| err.to_string())
}

// ---------------------------------------------------------------------------
// Hosted VST3 plugins
// ---------------------------------------------------------------------------
//
// A sibling surface to the CLAP one above rather than a generalisation of it.
// The two formats identify a plugin differently -- a CLAP id is a string, a VST3
// class id is 32 hex digits -- so a merged command would have had to accept both
// and decide which it was looking at. See `crate::vst3_bridge`.

/// Installed VST3 plugins, for the browser panel and the pickers.
///
/// The first call scans the standard VST3 directories, which opens every
/// installed module in this process. Pass `rescan: true` (the panel's Rescan
/// button) to walk them again.
///
/// **A module that hangs is remembered and skipped next time.** On a machine with
/// licence-protected plugins that is the difference between a panel that opens in
/// seconds and one that takes minutes and leaves a dialog on screen for each --
/// see `mydaw_plugin_host_vst3::blocklist`.
///
/// `async` so Tauri runs it off the main thread.
#[tauri::command]
pub async fn get_vst3_plugins(
    state: State<'_, AppState>,
    rescan: bool,
) -> Result<Vec<Vst3PluginInfoDto>, String> {
    Ok(state.vst3_plugins(rescan))
}

/// Scanned VST3 **audio effects**, for the master insert picker.
#[tauri::command]
pub async fn get_vst3_master_effects(
    state: State<'_, AppState>,
) -> Result<Vec<Vst3PluginInfoDto>, String> {
    Ok(state.vst3_master_effects())
}

/// Put a hosted VST3 instrument at the head of one track.
///
/// `modulePath` and `cid` come straight from an entry in [`get_vst3_plugins`].
/// Both travel because one module commonly exports several classes and a class
/// id alone would not say which copy was meant.
#[tauri::command]
pub async fn set_track_source_vst3(
    state: State<'_, AppState>,
    track: usize,
    module_path: String,
    cid: String,
) -> Result<MixerStateDto, String> {
    state
        .set_track_source_vst3(track, &module_path, &cid)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Put a hosted VST3 effect in the master insert.
///
/// The insert holds **one** plugin of either format, so this displaces a CLAP
/// effect that was there and destroys its instance -- see `crate::vst3_host`.
/// [`clear_master_effect`] empties the slot whichever format is in it.
#[tauri::command]
pub async fn set_master_effect_vst3(
    state: State<'_, AppState>,
    module_path: String,
    cid: String,
) -> Result<MixerStateDto, String> {
    state
        .set_master_effect_vst3(&module_path, &cid)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Open one hosted VST3 plugin's editor in its own native window.
///
/// `target` is `{"slot":"track","index":n}` or `{"slot":"master"}` -- the same
/// shape the CLAP commands take, because a slot is a place in the mixer and a
/// place in the mixer has no format.
#[tauri::command]
pub async fn open_vst3_gui(
    state: State<'_, AppState>,
    target: ClapTarget,
) -> Result<Vst3GuiStatusDto, String> {
    state.open_vst3_gui(target).map_err(|err| err.to_string())
}

/// Close it. Not an error when nothing is open.
#[tauri::command]
pub async fn close_vst3_gui(
    state: State<'_, AppState>,
    target: ClapTarget,
) -> Result<Vst3GuiStatusDto, String> {
    state.close_vst3_gui(target).map_err(|err| err.to_string())
}

/// Serialise a hosted VST3 plugin's component state, as base64.
///
/// An **empty** string means the plugin had nothing to save, which is a normal
/// answer. Only the component half is saved; see
/// `mydaw_plugin_host_vst3::LoadedVst3Plugin::save_state`.
#[tauri::command]
pub async fn save_vst3_state(
    state: State<'_, AppState>,
    target: ClapTarget,
) -> Result<String, String> {
    let bytes = state
        .save_vst3_state(target)
        .map_err(|err| err.to_string())?;
    Ok(crate::clap_bridge::to_base64(&bytes))
}

/// Restore a hosted VST3 plugin from a blob produced by [`save_vst3_state`].
#[tauri::command]
pub async fn load_vst3_state(
    state: State<'_, AppState>,
    target: ClapTarget,
    state_base64: String,
) -> Result<(), String> {
    let bytes = crate::clap_bridge::from_base64(&state_base64)?;
    state
        .load_vst3_state(target, bytes)
        .map_err(|err| err.to_string())
}

// ---------------------------------------------------------------------------
// The project file
// ---------------------------------------------------------------------------
//
// Four commands and one query, and the interesting decision is that **the file
// dialog lives here rather than in the webview**. The UI's backend abstraction
// exists so that every panel can run against a `MockBackend` in a plain browser
// (see `ui/src/api/backend.ts`); a panel that reached for `@tauri-apps/api`
// directly would work in the app and be a blank screen outside it. So the
// webview asks for "open a project" and Rust decides what that means, which also
// keeps "which extension is ours" in exactly one place.
//
// Each command takes an optional explicit `path`. With one, no dialog is shown
// at all -- which is what a headless run, a test and one day a command line
// need, and what makes the dialog a *convenience* rather than the only door.

/// Which project is open, and what went wrong opening it.
///
/// Called at startup and after every project command, so the window title and
/// the status bar can show the name.
#[tauri::command]
pub fn get_project_info(state: State<'_, AppState>) -> ProjectInfoDto {
    state.project_info()
}

/// Reset the session to a fresh default project: Drums and Synth, an empty
/// pattern, 130 bpm.
///
/// Everything is replaced -- every plugin instance is destroyed and the graph is
/// rebuilt -- so this waits for the main thread, hence `async`.
#[tauri::command]
pub async fn new_project(state: State<'_, AppState>) -> Result<ProjectInfoDto, String> {
    state.new_project().map_err(|err| err.to_string())
}

/// Open a `.mydaw` project, asking for one if `path` is not given.
///
/// Returns `null` when the user cancelled the dialog, which is not an error and
/// must not put anything in the status bar.
#[tauri::command]
pub async fn open_project(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: Option<String>,
) -> Result<Option<ProjectInfoDto>, String> {
    let Some(path) = path.map(PathBuf::from).or_else(|| pick_project(&app)) else {
        return Ok(None);
    };
    state
        .open_project(path)
        .map(Some)
        .map_err(|err| err.to_string())
}

/// Save the session.
///
/// With `saveAs` false and a project already on disk, this overwrites it with no
/// dialog -- the Ctrl+S case. Otherwise a save dialog is shown, and `null` comes
/// back if the user cancelled.
#[tauri::command]
pub async fn save_project(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    save_as: bool,
    path: Option<String>,
) -> Result<Option<ProjectInfoDto>, String> {
    let chosen = match path.map(PathBuf::from) {
        Some(explicit) => Some(explicit),
        None if !save_as => state
            .project_path()
            .or_else(|| pick_save_path(&app, &state)),
        None => pick_save_path(&app, &state),
    };
    let Some(path) = chosen else {
        return Ok(None);
    };
    state
        .save_project(path)
        .map(Some)
        .map_err(|err| err.to_string())
}

/// The native "open a project" dialog. `None` when the user cancelled.
///
/// Blocking, and safe here because a `#[tauri::command] async fn` runs on
/// Tauri's worker pool -- the dialog's own modal loop needs the main thread, and
/// blocking that thread waiting for it would be a deadlock.
fn pick_project(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .add_filter(PROJECT_FILTER, &[PROJECT_EXTENSION])
        .set_title("Open project")
        .blocking_pick_file()
        .and_then(|file| file.into_path().ok())
}

/// The native "save a project as" dialog, opened on the current project's
/// folder and name where there is one.
fn pick_save_path(app: &tauri::AppHandle, state: &AppState) -> Option<PathBuf> {
    use tauri_plugin_dialog::DialogExt;
    let current = state.project_path();
    let mut dialog = app
        .dialog()
        .file()
        .add_filter(PROJECT_FILTER, &[PROJECT_EXTENSION])
        .set_title("Save project as");
    dialog = match &current {
        Some(path) => {
            if let Some(parent) = path.parent() {
                dialog = dialog.set_directory(parent);
            }
            dialog.set_file_name(
                path.file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            )
        }
        None => dialog.set_file_name(format!("{UNTITLED}.{PROJECT_EXTENSION}")),
    };
    dialog.blocking_save_file().and_then(|file| {
        let path = file.into_path().ok()?;
        // A user who typed a bare name gets our extension, so a project cannot
        // end up as a file the Open dialog then filters out.
        Some(match path.extension() {
            Some(_) => path,
            None => path.with_extension(PROJECT_EXTENSION),
        })
    })
}

/// The label on the file dialogs' type filter.
const PROJECT_FILTER: &str = "MYDAW project";

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
