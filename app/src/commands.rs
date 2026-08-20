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
    AudioSourceDto, ClapPluginInfoDto, MeterFrameDto, PatternDto, SequencerStateDto, StreamInfoDto,
    Vst3PluginInfoDto, WaveformDto,
};
use crate::effects::{EffectInfoDto, InsertReadoutsDto, InsertTarget, ParamValueDto};
use crate::ipc_bench::{
    self, BenchPayload, BenchReport, BenchRunInput, EpochSample, EpochSync, StreamConfig,
};
use crate::library::{self, LibraryEntryDto, LibraryRootDto};
use crate::preview::SourcePreviewDto;
use crate::project::MixerStateDto;
use crate::project_bridge::ProjectInfoDto;
use crate::project_file::{PROJECT_EXTENSION, UNTITLED};
use crate::source::{Source, SourceDto};
use crate::state::AppState;
use crate::vst3_bridge::Vst3GuiStatusDto;
use mydaw_plugin_sequencer::{PlayMode, DEFAULT_CLIP_TICKS};
use mydaw_rt_core::StripId;

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

/// Move the playhead to an absolute sample position.
///
/// Samples rather than ticks, because that is what the transport counts and what
/// the meter stream reports back -- converting in two places would let the
/// playhead the UI draws and the playhead the engine plays disagree by a
/// rounding. `SequencerState` carries `ppq`, `ticksPerStep` and `sampleRate` so
/// the UI can do the conversion once, itself, from the same numbers.
#[tauri::command]
pub fn transport_seek(state: State<'_, AppState>, sample_pos: u64) -> Result<(), String> {
    state.seek(sample_pos).map_err(|err| err.to_string())
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

/// Channel pitch for an audio-clip or one-shot track, in cents (`-1200..=1200`).
#[tauri::command]
pub fn set_track_pitch(
    state: State<'_, AppState>,
    track: usize,
    cents: f32,
) -> Result<MixerStateDto, String> {
    state
        .set_track_pitch_cents(track, cents)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Channel fade in and out for an audio-clip or one-shot track, in ms.
///
/// A fade is a *time*, so it is milliseconds and not frames: the same 30 ms
/// means the same thing on a 44.1 kHz take and a 48 kHz one. Clamped to
/// `0..=10000`; a clip shorter than its fade-out fades from its first frame.
#[tauri::command]
pub fn set_track_fades(
    state: State<'_, AppState>,
    track: usize,
    fade_in_ms: f32,
    fade_out_ms: f32,
) -> Result<MixerStateDto, String> {
    state
        .set_track_fades(track, fade_in_ms, fade_out_ms)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// SMP region for an audio-clip or one-shot track, in the file's own frames.
///
/// `endFrames` of `0` means the file's last frame.
#[tauri::command]
pub fn set_track_sample_region(
    state: State<'_, AppState>,
    track: usize,
    start_frames: u64,
    end_frames: u64,
) -> Result<MixerStateDto, String> {
    state
        .set_track_sample_region(track, start_frames, end_frames)
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

/// Decode an audio file as a oneshot sampler and add a rack channel for it.
#[tauri::command]
pub async fn add_sample_channel(
    state: State<'_, AppState>,
    path: String,
) -> Result<MixerStateDto, String> {
    state
        .add_sample_channel(std::path::Path::new(&path))
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
// Buses, sends and insert chains
// ---------------------------------------------------------------------------
//
// Three kinds of command, and only one of them rebuilds the graph:
//
// | kind | cost |
// | --- | --- |
// | `set_send_level`, `set_send_pre`, `set_insert_param`, `set_insert_bypass`, and the bus fader/pan/mute/solo | one atomic store; the audio thread hears it on its next block with a 10 ms ramp |
// | `rename_bus`, `set_bus_color` | a string in the project; nothing crosses the RT wall at all |
// | `add_bus`, `remove_bus`, `add_send`, `remove_send`, `add_insert`, `remove_insert`, `move_insert` | a graph rebuild, through the same machinery `add_track` uses -- so they are `async`, because with a hosted plugin loaded they wait for a host's main thread |
//
// Every one of them succeeds with **no audio device open**, for the reason the
// pattern commands do: a mix is project data.

/// Every internal effect this build can insert, with the metadata a generic
/// knob panel renders from. Static: fetch once at startup.
#[tauri::command]
pub fn get_effects(state: State<'_, AppState>) -> Vec<EffectInfoDto> {
    state.effect_catalogue()
}

/// Add a bus, returning the whole mixer.
#[tauri::command]
pub async fn add_bus(
    state: State<'_, AppState>,
    name: Option<String>,
) -> Result<MixerStateDto, String> {
    state.add_bus(name).map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Remove a bus, **and every send at either end of it**.
///
/// Buses after it shift down, and every send naming one follows -- a bus index
/// is a position, and a send left pointing at the bus that moved into the gap
/// would silently reroute somebody's reverb.
#[tauri::command]
pub async fn remove_bus(state: State<'_, AppState>, bus: usize) -> Result<MixerStateDto, String> {
    state.remove_bus(bus).map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Rename a bus. Blank is refused.
#[tauri::command]
pub fn rename_bus(
    state: State<'_, AppState>,
    bus: usize,
    name: String,
) -> Result<MixerStateDto, String> {
    state
        .rename_bus(bus, &name)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Set or clear a bus's colour. `null` -- or a blank string -- clears it.
#[tauri::command]
pub fn set_bus_color(
    state: State<'_, AppState>,
    bus: usize,
    color: Option<String>,
) -> Result<MixerStateDto, String> {
    state
        .set_bus_color(bus, color)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Set a bus's fader, in dB.
#[tauri::command]
pub fn set_bus_gain(
    state: State<'_, AppState>,
    bus: usize,
    db: f32,
) -> Result<MixerStateDto, String> {
    state
        .set_bus_gain_db(bus, db)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Set a bus's pan, `-1.0`..=`1.0`.
#[tauri::command]
pub fn set_bus_pan(
    state: State<'_, AppState>,
    bus: usize,
    pan: f32,
) -> Result<MixerStateDto, String> {
    state.set_bus_pan(bus, pan).map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Mute or unmute a bus.
#[tauri::command]
pub fn set_bus_mute(
    state: State<'_, AppState>,
    bus: usize,
    muted: bool,
) -> Result<MixerStateDto, String> {
    state
        .set_bus_muted(bus, muted)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Solo or un-solo a bus. Tracks and buses share one solo scope.
#[tauri::command]
pub fn set_bus_solo(
    state: State<'_, AppState>,
    bus: usize,
    soloed: bool,
) -> Result<MixerStateDto, String> {
    state
        .set_bus_soloed(bus, soloed)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Add a send tap from a strip into a bus, at unity and post-fader.
///
/// `sourceKind` is `"track"` or `"bus"`. Refused for a duplicate pair, for a bus
/// sending to itself, and for anything that would feed a bus back into itself
/// through others -- the error names the whole loop.
#[tauri::command]
pub async fn add_send(
    state: State<'_, AppState>,
    source_kind: String,
    source_index: usize,
    dest: usize,
) -> Result<MixerStateDto, String> {
    let source = parse_strip(&source_kind, source_index)?;
    state
        .add_send(source, dest)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Remove a send tap, by its id.
#[tauri::command]
pub async fn remove_send(
    state: State<'_, AppState>,
    send_id: u32,
) -> Result<MixerStateDto, String> {
    state.remove_send(send_id).map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Set a send's level, linear, `0.0`..=`maxSendLevel`.
///
/// One atomic store, so this is what a knob drag sends per frame. Returns
/// nothing rather than the whole mixer for exactly that reason.
#[tauri::command]
pub fn set_send_level(state: State<'_, AppState>, send_id: u32, level: f32) -> Result<(), String> {
    state
        .set_send_level(send_id, level)
        .map_err(|err| err.to_string())
}

/// Move a send between pre- and post-fader.
///
/// Pre-fader is taken after the source's inserts and **before** its fader, pan
/// and mute, so a strip pulled to silence still feeds its bus. The change
/// crossfades over 10 ms rather than switching a route on a block boundary.
#[tauri::command]
pub fn set_send_pre(state: State<'_, AppState>, send_id: u32, pre: bool) -> Result<(), String> {
    state
        .set_send_pre(send_id, pre)
        .map_err(|err| err.to_string())
}

/// Add an internal effect to a chain, returning the whole mixer.
///
/// `target` is `"track"`, `"bus"` or `"master"`; `targetIndex` is ignored for
/// the master. `position` is where in the chain it lands, or the end when
/// absent.
#[tauri::command]
pub async fn add_insert(
    state: State<'_, AppState>,
    target: String,
    target_index: usize,
    type_id: String,
    position: Option<usize>,
) -> Result<MixerStateDto, String> {
    let target = parse_insert_target(&target, target_index)?;
    state
        .add_insert(target, &type_id, position)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Remove an insert from a chain.
#[tauri::command]
pub async fn remove_insert(
    state: State<'_, AppState>,
    target: String,
    target_index: usize,
    index: usize,
) -> Result<MixerStateDto, String> {
    let target = parse_insert_target(&target, target_index)?;
    state
        .remove_insert(target, index)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Move an insert within its chain, so that it ends up at `to`.
#[tauri::command]
pub async fn move_insert(
    state: State<'_, AppState>,
    target: String,
    target_index: usize,
    from: usize,
    to: usize,
) -> Result<MixerStateDto, String> {
    let target = parse_insert_target(&target, target_index)?;
    state
        .move_insert(target, from, to)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// Set one insert parameter, returning **what was actually stored**.
///
/// One atomic store, so this is what a knob drag sends per frame. The answer is
/// the sanitised value -- clamped into range and snapped to the parameter's step
/// grid -- so a UI can put its knob where the engine actually is.
#[tauri::command]
pub fn set_insert_param(
    state: State<'_, AppState>,
    target: String,
    target_index: usize,
    index: usize,
    param_id: String,
    value: f32,
) -> Result<f32, String> {
    let target = parse_insert_target(&target, target_index)?;
    state
        .set_insert_param(target, index, &param_id, value)
        .map_err(|err| err.to_string())
}

/// Turn one insert's bypass on or off.
///
/// A parameter like any other -- each effect crossfades itself over 10 ms and is
/// bit-transparent once settled -- offered by name because every strip draws the
/// button.
#[tauri::command]
pub fn set_insert_bypass(
    state: State<'_, AppState>,
    target: String,
    target_index: usize,
    index: usize,
    bypassed: bool,
) -> Result<MixerStateDto, String> {
    let target = parse_insert_target(&target, target_index)?;
    state
        .set_insert_bypass(target, index, bypassed)
        .map_err(|err| err.to_string())?;
    Ok(state.mixer_state())
}

/// One insert's live meter values.
///
/// **Separate from the parameter values on purpose**: a gain-reduction bar wants
/// to move at frame rate, and sending twenty knob positions sixty times a second
/// to animate one bar is the mistake this split exists to prevent.
#[tauri::command]
pub fn get_insert_readouts(
    state: State<'_, AppState>,
    target: String,
    target_index: usize,
    index: usize,
) -> Result<Vec<ParamValueDto>, String> {
    let target = parse_insert_target(&target, target_index)?;
    state
        .insert_readouts(target, index)
        .map_err(|err| err.to_string())
}

/// Every meter on one chain, in chain order. Inserts that publish none appear
/// with an empty list, so the answer's length is the chain's length.
#[tauri::command]
pub fn get_chain_readouts(
    state: State<'_, AppState>,
    target: String,
    target_index: usize,
) -> Result<Vec<InsertReadoutsDto>, String> {
    let target = parse_insert_target(&target, target_index)?;
    state.chain_readouts(target).map_err(|err| err.to_string())
}

/// Arm the analysis tap on one insert, or pass nulls to silence it.
#[tauri::command]
pub fn watch_spectrum(
    state: State<'_, AppState>,
    target: Option<String>,
    target_index: usize,
    index: Option<usize>,
) -> Result<(), String> {
    let dest = match target.as_deref() {
        None => None,
        Some(kind) => Some(parse_insert_target(kind, target_index)?),
    };
    state
        .watch_spectrum(dest, index)
        .map_err(|err| err.to_string())
}

/// Latest hop from the armed tap, or `null` when nothing is watched.
#[tauri::command]
pub fn get_insert_spectrum(state: State<'_, AppState>) -> Option<crate::effects::SpectrumDto> {
    state.insert_spectrum()
}

/// EQ band-solo of the source. `band` is a prefix, or `null` to stop.
#[tauri::command]
pub fn set_insert_listen(
    state: State<'_, AppState>,
    target: String,
    target_index: usize,
    index: usize,
    band: Option<String>,
) -> Result<(), String> {
    let target = parse_insert_target(&target, target_index)?;
    state
        .set_insert_listen(target, index, band.as_deref())
        .map_err(|err| err.to_string())
}

/// `"track"` / `"bus"` plus an index, as the webview sends a send's source.
fn parse_strip(kind: &str, index: usize) -> Result<StripId, String> {
    match kind {
        "track" => Ok(StripId::Track(index)),
        "bus" => Ok(StripId::Bus(index)),
        other => Err(format!(
            "'{other}' is not a place a send can come from; expected 'track' or 'bus'"
        )),
    }
}

/// `"track"` / `"bus"` / `"master"` plus an index, as the webview sends a chain.
fn parse_insert_target(kind: &str, index: usize) -> Result<InsertTarget, String> {
    InsertTarget::parse(kind, index).map_err(|err| err.to_string())
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

/// Repeat the arrangement, or stop at the end of the last clip.
///
/// Song mode only: a pattern is a loop by definition and PAT ignores this.
#[tauri::command]
pub fn set_loop(state: State<'_, AppState>, looping: bool) -> Result<SequencerStateDto, String> {
    state.set_looping(looping).map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// The whole sequencer: patterns, the arrangement, the tempo, the mode and the
/// tick geometry. Called once at startup.
#[tauri::command]
pub fn get_pattern(state: State<'_, AppState>) -> SequencerStateDto {
    state.sequencer_state()
}

/// The content of **one** pattern, by id.
///
/// `SequencerStateDto` carries the whole set as [`PatternSummaryDto`]s -- counts
/// and no content -- plus the active pattern in full, which is everything the
/// rack and the roll need and nothing more. The playlist needs more: a clip
/// draws a miniature of the pattern it plays, and most clips on a timeline are
/// not the pattern being edited.
///
/// So: one pattern at a time, asked for by the view that wants it and cached
/// there, rather than fattening every sequencer reply with the content of every
/// pattern for the sake of a few boxes of marks. A pattern is a few kilobytes;
/// the whole set on every step toggle would not be.
///
/// # Errors
/// A sentence naming the id, when the set has no such pattern.
#[tauri::command]
pub fn get_pattern_content(
    state: State<'_, AppState>,
    pattern_id: u64,
) -> Result<PatternDto, String> {
    state
        .sequencer()
        .patterns()
        .get(pattern_id)
        .map(PatternDto::from)
        .ok_or_else(|| format!("there is no pattern {pattern_id}"))
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

// ------------------------------------------------------ audio clips and files

/// Decode an audio file into memory and give it a source id.
///
/// WAV, FLAC and Ogg Vorbis. The whole file goes into RAM, capped at
/// [`MAX_SOURCE_SECONDS`](mydaw_plugin_sequencer::MAX_SOURCE_SECONDS); a file
/// past that is refused with a message rather than truncated silently.
///
/// Loading a path that is already loaded returns the source that is already
/// there rather than a second copy of it, so dragging one loop onto four lanes
/// decodes once.
///
/// Returns the source, **not** the whole sequencer state: nothing about the
/// arrangement changed. Placing it is [`add_audio_clip`].
#[tauri::command]
pub fn load_audio_source(
    state: State<'_, AppState>,
    path: String,
) -> Result<AudioSourceDto, String> {
    let source = state
        .load_audio_source(std::path::Path::new(&path))
        .map_err(|err| err.to_string())?;
    Ok(AudioSourceDto::of(&source, state.bpm()))
}

/// Ask for an audio file with the native dialog and load it.
///
/// `null` when the user cancelled, which is not an error and must not put
/// anything in the status bar -- the same shape [`open_project`] answers a
/// cancel with.
///
/// The dialog lives here rather than in the webview for the reason the project
/// dialogs do: the dialog plugin only exists inside the app, so a panel that
/// raised one directly would be a blank screen in a plain browser and the mock
/// would have nothing to stand in for. It also keeps "which formats can we
/// decode" in one place -- [`load_audio_source`] and this filter cannot drift.
///
/// `async` because the dialog's own modal loop needs the main thread; blocking
/// that thread waiting for it would be a deadlock.
#[tauri::command]
pub async fn open_audio_file(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<AudioSourceDto>, String> {
    let Some(path) = pick_audio_file(&app) else {
        return Ok(None);
    };
    let source = state
        .load_audio_source(&path)
        .map_err(|err| err.to_string())?;
    Ok(Some(AudioSourceDto::of(&source, state.bpm())))
}

/// The native "open an audio file" dialog. `None` when the user cancelled.
fn pick_audio_file(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .add_filter(AUDIO_FILTER, AUDIO_EXTENSIONS)
        .set_title("Load audio file")
        .blocking_pick_file()
        .and_then(|file| file.into_path().ok())
}

/// The native "add a sample folder" dialog. `None` when the user cancelled.
fn pick_library_folder(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .set_title("Add sample folder")
        .blocking_pick_folder()
        .and_then(|file| file.into_path().ok())
}

fn library_json(app: &tauri::AppHandle) -> PathBuf {
    use tauri::Manager;
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("mydaw"))
        .join("library.json")
}

fn packs_dir(app: &tauri::AppHandle) -> PathBuf {
    use tauri::Manager;
    let resource = app.path().resource_dir().ok();
    library::resolve_packs_dir(resource.as_deref())
}

/// The label on the audio dialog's type filter.
const AUDIO_FILTER: &str = "Audio files";

/// What [`load_audio_source`] can decode, and therefore what the dialog offers.
const AUDIO_EXTENSIONS: &[&str] = mydaw_plugin_sequencer::AUDIO_EXTENSIONS;

/// Every audio file the session has loaded, by ascending id.
#[tauri::command]
pub fn get_audio_sources(state: State<'_, AppState>) -> Vec<AudioSourceDto> {
    let bpm = state.bpm();
    state
        .audio_sources()
        .list()
        .iter()
        .map(|source| AudioSourceDto::of(source, bpm))
        .collect()
}

/// Drop an audio file from memory.
///
/// **Refused** while any clip still plays it, and the message says how many --
/// unloading it anyway would silence part of the arrangement without saying so.
/// Removing the last clip does *not* unload the file on its own: a user who
/// deletes a clip and draws it again a second later should not pay for a second
/// decode.
#[tauri::command]
pub fn unload_audio_source(
    state: State<'_, AppState>,
    source_id: u64,
) -> Result<Vec<AudioSourceDto>, String> {
    state
        .unload_audio_source(source_id)
        .map_err(|err| err.to_string())?;
    let bpm = state.bpm();
    Ok(state
        .audio_sources()
        .list()
        .iter()
        .map(|source| AudioSourceDto::of(source, bpm))
        .collect())
}

/// A source's waveform as min/max pairs, for drawing a clip.
///
/// `targetBins` is a request, not a promise: a file with less detail than that
/// returns what it has, and anything past
/// [`MAX_WAVEFORM_BINS`](mydaw_plugin_sequencer::MAX_WAVEFORM_BINS) is clamped.
/// Absent means 1024, which is about one pair per pixel for a clip drawn across
/// a laptop screen.
#[tauri::command]
pub fn get_audio_waveform(
    state: State<'_, AppState>,
    source_id: u64,
    target_bins: Option<usize>,
) -> Result<WaveformDto, String> {
    let source = state
        .audio_sources()
        .get(source_id)
        .ok_or_else(|| format!("there is no audio source {source_id}"))?;
    Ok(WaveformDto::of(&source, target_bins.unwrap_or(1_024)))
}

/// One hit of a built-in or sampled source, as min/max bins.
///
/// Hosted plugins are refused: they draw their own editor. `targetBins` absent
/// means 512, which is about one pair per pixel in the sample window.
#[tauri::command]
pub fn preview_source(
    state: State<'_, AppState>,
    source_id: String,
    target_bins: Option<usize>,
) -> Result<SourcePreviewDto, String> {
    state
        .preview_source(&source_id, target_bins.unwrap_or(512))
        .map_err(|err| err.to_string())
}

/// The sample library roots: bundled Packs, then extra-search folders.
#[tauri::command]
pub fn get_library_roots(app: tauri::AppHandle) -> Vec<LibraryRootDto> {
    let settings = library::load_settings(&library_json(&app));
    library::roots(&packs_dir(&app), &settings)
}

/// Ask for a folder with the native dialog and remember it as an extra-search
/// root. `null` when the user cancelled.
#[tauri::command]
pub async fn add_library_folder(
    app: tauri::AppHandle,
) -> Result<Option<Vec<LibraryRootDto>>, String> {
    let Some(path) = pick_library_folder(&app) else {
        return Ok(None);
    };
    let settings_path = library_json(&app);
    let mut settings = library::load_settings(&settings_path);
    library::add_folder(&mut settings, path).map_err(|err| err.to_string())?;
    library::save_settings(&settings_path, &settings).map_err(|err| err.to_string())?;
    Ok(Some(library::roots(&packs_dir(&app), &settings)))
}

/// Forget an extra-search folder. Packs cannot be removed.
#[tauri::command]
pub fn remove_library_folder(
    app: tauri::AppHandle,
    id: String,
) -> Result<Vec<LibraryRootDto>, String> {
    let settings_path = library_json(&app);
    let mut settings = library::load_settings(&settings_path);
    library::remove_folder(&mut settings, &id).map_err(|err| err.to_string())?;
    library::save_settings(&settings_path, &settings).map_err(|err| err.to_string())?;
    Ok(library::roots(&packs_dir(&app), &settings))
}

/// One level of a library root: directories and audio files.
#[tauri::command]
pub fn list_library_dir(
    app: tauri::AppHandle,
    root_id: String,
    rel_path: String,
) -> Result<Vec<LibraryEntryDto>, String> {
    let settings = library::load_settings(&library_json(&app));
    let roots = library::roots(&packs_dir(&app), &settings);
    let root = roots
        .iter()
        .find(|root| root.id == root_id)
        .ok_or_else(|| format!("there is no library folder '{root_id}'"))?;
    library::list_dir(std::path::Path::new(&root.path), &rel_path).map_err(|err| err.to_string())
}

/// Longest OS-dropped file the webview may hand over, in bytes. The bytes
/// travel in one JSON IPC message; past this the message stalls the webview.
const MAX_IMPORT_BYTES: usize = 100 * 1024 * 1024;

/// Copy a file dropped from the OS into the app's imports folder and answer
/// with its absolute path.
///
/// WebView2 delivers an OS drop as a name and bytes, never a path, and Tauri's
/// native drop handler is off so internal HTML5 drags work (`dragDropEnabled:
/// false` — see `ui/src/lib/dnd.ts`). Writing the bytes next to `library.json`
/// gives the decode pipeline the real path it already understands.
#[tauri::command]
pub fn import_dropped_audio(
    app: tauri::AppHandle,
    file_name: String,
    bytes: Vec<u8>,
) -> Result<String, String> {
    use tauri::Manager;

    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(format!(
            "{file_name} is {} MB — dropped files are capped at {} MB",
            bytes.len() / (1024 * 1024),
            MAX_IMPORT_BYTES / (1024 * 1024)
        ));
    }
    // A browser drop reports a bare name, but nothing stops a payload from
    // carrying separators; keep only the final component.
    let name = std::path::Path::new(&file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "that drop has no usable file name".to_owned())?;
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
        })
        .ok_or_else(|| format!("{name} is not an audio format this build can decode"))?
        .to_owned();
    let stem = std::path::Path::new(name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| "that drop has no usable file name".to_owned())?;

    let dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("mydaw"))
        .join("imports");
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))?;

    // Never overwrite: a second drop of a different `kick.wav` must not
    // silently replace the one clips already play.
    let mut target = dir.join(name);
    let mut suffix = 1_u32;
    while target.exists() {
        target = dir.join(format!("{stem}-{suffix}.{ext}"));
        suffix += 1;
    }
    std::fs::write(&target, &bytes)
        .map_err(|err| format!("could not write {}: {err}", target.display()))?;
    Ok(target.to_string_lossy().into_owned())
}

/// Audition a file without loading it into the project.
#[tauri::command]
pub fn preview_audio_file(state: State<'_, AppState>, path: String) -> Result<(), String> {
    state
        .preview_audio_file(std::path::Path::new(&path))
        .map_err(|err| err.to_string())
}

/// Place an audio file on the timeline.
///
/// `start` and `length` are in **ticks**; `offsetFrames` is in frames of the
/// *file's own* rate, because it is a position in a recording rather than in the
/// song and must not move when the tempo changes.
///
/// `length` absent means "the whole file at the current tempo". The audio is
/// **not** stretched to fit: a clip longer than its file ends when the file
/// does, and a shorter one cuts it off.
#[tauri::command]
pub fn add_audio_clip(
    state: State<'_, AppState>,
    source_id: u64,
    track: usize,
    start: u64,
    length: Option<u64>,
    offset_frames: Option<u64>,
    gain_db: Option<f32>,
) -> Result<SequencerStateDto, String> {
    let length = match length {
        Some(length) => length,
        None => state
            .audio_clip_default_length(source_id)
            .map_err(|err| err.to_string())?,
    };
    let offset = offset_frames.unwrap_or(0);
    let gain = gain_db.unwrap_or(0.0);
    state
        .ensure_audio_clip_channel(source_id)
        .map_err(|err| err.to_string())?;
    state
        .edit_playlist(|list| list.add_audio_clip(source_id, track, start, length, offset, gain))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Set an audio clip's trim, in dB. `0.0` is unity.
///
/// Refused for a pattern clip, which has no gain of its own -- its channels have
/// faders.
#[tauri::command]
pub fn set_clip_gain(
    state: State<'_, AppState>,
    clip_id: u64,
    gain_db: f32,
) -> Result<SequencerStateDto, String> {
    state
        .edit_playlist(|list| list.set_clip_gain(clip_id, gain_db))
        .map_err(|err| err.to_string())?;
    Ok(state.sequencer_state())
}

/// Set how far into its file an audio clip begins, in the file's own frames.
#[tauri::command]
pub fn set_clip_offset(
    state: State<'_, AppState>,
    clip_id: u64,
    offset_frames: u64,
) -> Result<SequencerStateDto, String> {
    state
        .edit_playlist(|list| list.set_clip_offset(clip_id, offset_frames))
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
/// The first call scans the standard VST3 directories. Pass `rescan: true` (the
/// panel's Rescan button) to walk them again.
///
/// **No module is ever opened in this process.** A module MYDAW has not seen
/// before is opened in a child process which is killed if it will not answer, so
/// a plugin that crashes or puts up a licence dialog costs one entry in the
/// plugin list rather than the DAW; one that has not changed since the last scan
/// is answered from a metadata cache and not opened at all. See
/// `mydaw_plugin_scan`.
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
