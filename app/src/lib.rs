//! WAVE desktop app: Tauri commands around the engine, the audio device, the
//! sample library, VST3 plugins and the live session the `wave` CLI talks to.

mod actions;
mod cli_shim;
mod keys;
mod library;
mod models;
mod plugins;
mod recovery;
mod settings;
mod updater;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody, Response};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use wave_audio_io::{AudioConfig, AudioIo, AudioStatus, LatencyReport};
use wave_engine::{Engine, Op, Project, StateDto};
use wave_midi_in::{MidiDevice, MidiInput, MidiStatus};

pub use cli_shim::CliStatus;

pub struct App {
    pub engine: Arc<Engine>,
    pub audio: AudioIo,
    pub midi: Arc<MidiInput>,
    pub plugins: Arc<plugins::Plugins>,
    pub settings: Mutex<settings::Settings>,
    pub status: Mutex<AudioStatus>,
    pub cli: Mutex<CliStatus>,
    /// Set while an offline bounce is running; flipped by `export_cancel`.
    pub export_cancel: AtomicBool,
    /// Why the installed RMVPE model could not be used, if it could not.
    pub model_error: Mutex<Option<String>>,
    session: Mutex<Option<wave_session::Server>>,
    /// WAVE Keys piano: where it is and whether it is loaded.
    pub keys: keys::Keys,
    /// The update found by the last check and its downloaded installer.
    pub updater: updater::State,
    /// Set once the window exists (events to the UI).
    pub handle: std::sync::OnceLock<AppHandle>,
    /// Crash-recovery copy of a project that was never saved.
    pub recovery: recovery::Recovery,
}

impl App {
    pub fn audio_status(&self) -> AudioStatus {
        let s = self.audio.status();
        *self.status.lock() = s.clone();
        s
    }
}

type St<'a> = State<'a, Arc<App>>;
type R<T> = Result<T, String>;

fn es<E: ToString>(e: E) -> String {
    e.to_string()
}

// ---------------------------------------------------------------- project & undo

#[tauri::command]
async fn get_state(app: St<'_>) -> R<StateDto> {
    Ok(app.engine.state())
}

#[tauri::command]
async fn apply_ops(app: St<'_>, ops: Vec<Op>, transient: Option<bool>) -> R<Value> {
    let rev = app.engine.apply(&ops, transient.unwrap_or(false), None).map_err(es)?;
    Ok(json!({ "revision": rev }))
}

#[tauri::command]
async fn begin_gesture(app: St<'_>) -> R<()> {
    app.engine.begin_gesture();
    Ok(())
}

#[tauri::command]
async fn end_gesture(app: St<'_>) -> R<()> {
    app.engine.end_gesture();
    Ok(())
}

#[tauri::command]
async fn undo(app: St<'_>) -> R<StateDto> {
    app.engine.undo();
    Ok(app.engine.state())
}

#[tauri::command]
async fn redo(app: St<'_>) -> R<StateDto> {
    app.engine.redo();
    Ok(app.engine.state())
}

#[tauri::command]
async fn fx_catalog() -> R<wave_engine::catalog::Catalog> {
    Ok(wave_engine::catalog::catalog())
}

// ---------------------------------------------------------------- transport & telemetry

#[tauri::command]
async fn transport_play(app: St<'_>, from_beat: f64) -> R<()> {
    app.engine.play(from_beat);
    Ok(())
}

#[tauri::command]
async fn transport_stop(app: St<'_>) -> R<()> {
    let a = app.inner().clone();
    let st = tauri::async_runtime::spawn_blocking(move || {
        a.engine.stop();
        let t = a.engine.state().transport;
        (!t.rec && !t.monitor).then(|| a.audio.set_input(false))
    })
    .await
    .map_err(es)?;
    if let Some(st) = st {
        *app.status.lock() = st;
    }
    Ok(())
}

#[tauri::command]
async fn transport_set(app: St<'_>, metro: Option<bool>, count_in: Option<bool>, rec: Option<bool>, monitor: Option<bool>, start_beat: Option<f64>) -> R<wave_engine::TransportDto> {
    let e = app.engine.clone();
    let t = tauri::async_runtime::spawn_blocking(move || {
        if let Some(b) = start_beat {
            e.set_start(b);
        }
        e.set_transport(metro, count_in, rec, monitor)
    })
    .await
    .map_err(es)?;
    // the mic is opened only while recording or monitoring is on (Bluetooth headphones
    // drop to the low-quality headset profile whenever a mic stream is open). It opens and
    // closes without touching the output, so a punch-in keeps the song playing; closing
    // still waits for the transport to stop (a punch-out may be followed by another punch-in).
    if rec.is_some() || monitor.is_some() {
        let need = t.rec || t.monitor;
        if need || !t.playing {
            let a = app.inner().clone();
            let st = tauri::async_runtime::spawn_blocking(move || a.audio.set_input(need)).await.map_err(es)?;
            *app.status.lock() = st;
        }
    }
    Ok(t)
}

#[tauri::command]
async fn telemetry_subscribe(app: St<'_>, channel: Channel<InvokeResponseBody>) -> R<()> {
    app.engine.set_frame_sink(Some(Box::new(move |bytes| {
        let _ = channel.send(InvokeResponseBody::Raw(bytes));
    })));
    Ok(())
}

#[tauri::command]
async fn watch_fx(app: St<'_>, ids: Vec<u64>) -> R<()> {
    app.engine.watch_fx(&ids);
    Ok(())
}

// ---------------------------------------------------------------- audio sources

#[tauri::command]
async fn import_audio(app: St<'_>, paths: Vec<String>, track: Option<u32>, beat: Option<f64>) -> R<Value> {
    let a = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || actions::import(&a, &json!({ "paths": paths, "track": track, "beat": beat })))
        .await
        .map_err(es)?
}

#[tauri::command]
async fn source_peaks(app: St<'_>, id: u64, norm: Option<bool>, rev: Option<bool>) -> R<Response> {
    let p = app.engine.source_peaks(id, norm.unwrap_or(false), rev.unwrap_or(false)).ok_or("no such source")?;
    let mut b = Vec::with_capacity(p.len() * 4);
    for x in p {
        b.extend_from_slice(&x.to_le_bytes());
    }
    Ok(Response::new(b))
}

#[tauri::command]
async fn pitch_analyze(app: St<'_>, clip_id: u64) -> R<std::sync::Arc<wave_engine::pitch::PitchAnalysis>> {
    let e = app.engine.clone();
    tauri::async_runtime::spawn_blocking(move || e.pitch_analysis(clip_id)).await.map_err(es)?.map_err(es)
}

#[tauri::command]
async fn pitch_target(app: St<'_>, clip_id: u64, reference: Option<wave_engine::RefTune>) -> R<std::sync::Arc<wave_engine::reference::PitchTarget>> {
    let e = app.engine.clone();
    tauri::async_runtime::spawn_blocking(move || e.pitch_target(clip_id, reference)).await.map_err(es)?.map_err(es)
}

#[tauri::command]
async fn f0_model_status(app: St<'_>) -> R<models::ModelStatus> {
    Ok(models::status(&app.engine, app.model_error.lock().clone()))
}

#[tauri::command]
async fn f0_model_install(app: St<'_>, handle: AppHandle) -> R<models::ModelStatus> {
    let a = app.inner().clone();
    let h = handle.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        models::install(|done, total| {
            let _ = h.emit("model-progress", json!({ "id": "rmvpe", "done": done, "total": total }));
        })?;
        models::activate(&a.engine).map(|_| ())
    })
    .await
    .map_err(es)?;
    *app.model_error.lock() = res.as_ref().err().cloned();
    if let Err(e) = &res {
        let _ = handle.emit("model-progress", json!({ "id": "rmvpe", "done": 0, "total": 0, "error": e }));
    }
    res.map(|_| models::status(&app.engine, None))
}

#[tauri::command]
async fn music_analyze(app: St<'_>, clip_id: Option<u64>, exclude_fx: Option<u64>) -> R<wave_engine::music::MusicAnalysis> {
    let e = app.engine.clone();
    tauri::async_runtime::spawn_blocking(move || e.music_analysis(clip_id, exclude_fx)).await.map_err(es)?.map_err(es)
}

/// "Auto wokal": measures a vocal clip (and the beat's key) and returns the calibrated
/// insert chain for its track. Read-only: the UI applies the chain with `apply_ops`.
/// `bake_rider: true`: the rider is left out of `chain` and returned as `riderBake`, for
/// `bake_rider` (then the chain goes on the insert with no live look-ahead delay).
#[tauri::command]
async fn auto_vocal_plan(app: St<'_>, clip_id: u64, beat_clip_id: Option<u64>, style: Option<wave_engine::autovocal::Style>, bake_rider: Option<bool>) -> R<wave_engine::autovocal::AutoVocalPlan> {
    let e = app.engine.clone();
    tauri::async_runtime::spawn_blocking(move || e.auto_vocal_plan(clip_id, beat_clip_id, style.unwrap_or_default(), bake_rider.unwrap_or(false))).await.map_err(es)?.map_err(es)
}

/// Renders the Vocal Rider (`params`, e.g. a plan's `riderBake.params`) into an audio clip's
/// audio as a new source and swaps the clip onto it (one undo step; the original source stays).
#[tauri::command]
async fn bake_rider(app: St<'_>, clip_id: u64, params: std::collections::BTreeMap<String, f64>) -> R<Value> {
    let e = app.engine.clone();
    let (source, clip) = tauri::async_runtime::spawn_blocking(move || e.bake_rider(clip_id, &params)).await.map_err(es)?.map_err(es)?;
    Ok(json!({ "sourceId": source, "clipId": clip }))
}

#[tauri::command]
async fn clip_peaks(app: St<'_>, clip_id: u64) -> R<Response> {
    let e = app.engine.clone();
    let p = tauri::async_runtime::spawn_blocking(move || e.clip_peaks(clip_id)).await.map_err(es)?.ok_or("no such clip")?;
    let mut b = Vec::with_capacity(p.len() * 4);
    for x in p {
        b.extend_from_slice(&x.to_le_bytes());
    }
    Ok(Response::new(b))
}

#[tauri::command]
async fn trim_clip(app: St<'_>, clip_id: u64) -> R<Value> {
    let e = app.engine.clone();
    let id = tauri::async_runtime::spawn_blocking(move || e.trim_clip(clip_id)).await.map_err(es)?.map_err(es)?;
    Ok(json!({ "sourceId": id }))
}

#[tauri::command]
async fn flatten_comp(app: St<'_>, clip_id: u64) -> R<Value> {
    let e = app.engine.clone();
    let id = tauri::async_runtime::spawn_blocking(move || e.flatten_comp(clip_id)).await.map_err(es)?.map_err(es)?;
    Ok(json!({ "sourceId": id }))
}

#[tauri::command]
async fn comp_audition(app: St<'_>, comp_id: u64, take_id: Option<u64>) -> R<()> {
    app.engine.audition(comp_id, take_id).map_err(es)
}

#[tauri::command]
async fn preview_clip(app: St<'_>, clip_id: u64) -> R<()> {
    app.engine.preview_clip(clip_id).map_err(es)
}

#[tauri::command]
async fn preview_file(app: St<'_>, path: String) -> R<()> {
    let e = app.engine.clone();
    tauri::async_runtime::spawn_blocking(move || e.preview_file(&PathBuf::from(path))).await.map_err(es)?.map_err(es)
}

#[tauri::command]
async fn note_audition(app: St<'_>, ch: u64, key: Option<u8>, vel: Option<f32>, pattern: Option<u64>) -> R<()> {
    app.engine.note_audition(ch, key, vel.unwrap_or(0.8), pattern).map_err(es)
}

/// Computer keyboard as a piano: key down / up on a Channel Rack channel.
#[tauri::command]
async fn typing_note(app: St<'_>, ch: u64, key: u8, vel: Option<f32>, on: bool) -> R<()> {
    app.engine.typing_note(ch, key, vel.unwrap_or(0.8), on);
    Ok(())
}

#[tauri::command]
async fn pattern_play(app: St<'_>, pattern: Option<u64>) -> R<()> {
    app.engine.pattern_play(pattern).map_err(es)
}

#[tauri::command]
async fn preview_stop(app: St<'_>) -> R<()> {
    app.engine.preview_stop();
    Ok(())
}

// ---------------------------------------------------------------- audio device

#[tauri::command]
async fn audio_hosts() -> R<Vec<wave_audio_io::HostInfo>> {
    Ok(wave_audio_io::hosts())
}

#[tauri::command]
async fn audio_devices(host: String) -> R<wave_audio_io::DeviceList> {
    tauri::async_runtime::spawn_blocking(move || wave_audio_io::devices(&host)).await.map_err(es)
}

#[tauri::command]
async fn audio_get_config(app: St<'_>) -> R<AudioStatus> {
    Ok(app.audio_status())
}

#[tauri::command]
async fn audio_set_config(app: St<'_>, handle: AppHandle, mut config: AudioConfig) -> R<AudioStatus> {
    let a = app.inner().clone();
    let st = tauri::async_runtime::spawn_blocking(move || {
        // a config sent without `recOffsetMs` keeps the calibration (a device change does
        // not lose it); changing only the offset does not restart the streams
        if config.rec_offset_ms.is_none() {
            config.rec_offset_ms = a.settings.lock().audio.rec_offset_ms;
        }
        let st = a.audio.start(config.clone());
        let mut s = a.settings.lock();
        s.audio = config;
        s.save();
        *a.status.lock() = st.clone();
        st
    })
    .await
    .map_err(es)?;
    let _ = handle.emit("audio-status", &st);
    Ok(st)
}

/// Measures the recording round trip with clicks played on the output and recorded back
/// (microphone at the speaker, or a loopback cable). Opens the input for the ~6 s it takes
/// and closes it again unless recording or monitoring needs it. The suggested offset is
/// what `recOffsetMs` should be; nothing is saved here.
#[tauri::command]
async fn audio_measure_latency(app: St<'_>) -> R<LatencyReport> {
    let a = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if a.engine.state().transport.playing {
            return Err("Stop playback before measuring the latency.".to_string());
        }
        *a.status.lock() = a.audio.set_input(true);
        let res = a.audio.measure_latency();
        let t = a.engine.state().transport;
        if !t.rec && !t.monitor {
            *a.status.lock() = a.audio.set_input(false);
        }
        res
    })
    .await
    .map_err(es)?
}

// ---------------------------------------------------------------- MIDI keyboard

/// The input ports the OS lists right now (rescanned on every call).
#[tauri::command]
async fn midi_list_devices(app: St<'_>) -> R<Vec<MidiDevice>> {
    let a = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || a.midi.list()).await.map_err(es)
}

/// Chooses the keyboard (`None` = none) and opens it; the choice is saved and reopened at startup.
#[tauri::command]
async fn midi_select_device(app: St<'_>, id: Option<String>) -> R<MidiStatus> {
    let a = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = a.midi.select(id.as_deref(), None);
        let mut s = a.settings.lock();
        s.midi_device = st.selected.clone();
        s.save();
        st
    })
    .await
    .map_err(es)
}

#[tauri::command]
async fn midi_status(app: St<'_>) -> R<MidiStatus> {
    Ok(app.midi.status())
}

/// Which Channel Rack channel (instrument) the keyboard plays; `None` = the first pitched one.
#[tauri::command]
async fn midi_set_target(app: St<'_>, channel: Option<u64>) -> R<MidiStatus> {
    let st = app.midi.set_target(channel);
    app.engine.midi_configure(channel, st.connected.is_some());
    Ok(st)
}

// ---------------------------------------------------------------- library

#[tauri::command]
async fn library_list(app: St<'_>) -> R<Value> {
    Ok(json!({ "kits": library::kits(), "folders": app.settings.lock().folders }))
}

/// WAVE Keys: is the piano on this computer, and is it loaded?
#[tauri::command]
async fn keys_status(app: St<'_>) -> R<keys::KeysStatus> {
    Ok(keys::status(&app))
}

#[tauri::command]
async fn library_install(app: St<'_>, handle: AppHandle, id: String) -> R<()> {
    let h = handle.clone();
    let kid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        library::install(&kid, |done, total| {
            let _ = h.emit("library-progress", json!({ "id": kid, "done": done, "total": total }));
        })
    })
    .await
    .map_err(es)?;
    if let Err(e) = &res {
        let _ = handle.emit("library-progress", json!({ "id": id, "done": 0, "total": 0, "error": e }));
    } else if id == wave_engine::keys::KIT_ID {
        // the piano just arrived: Keys channels start playing
        keys::ensure(app.inner(), app.engine.uses_keys());
    }
    res
}

#[tauri::command]
async fn library_files(kit: Option<String>, folder: Option<String>) -> R<Vec<library::FileInfo>> {
    tauri::async_runtime::spawn_blocking(move || match (kit, folder) {
        (Some(k), _) => library::kit_files(&k),
        (None, Some(f)) => Ok(library::folder_files(&PathBuf::from(f))),
        _ => Err("pass `kit` or `folder`".to_string()),
    })
    .await
    .map_err(es)?
}

#[tauri::command]
async fn library_add_folder(app: St<'_>, handle: AppHandle) -> R<Vec<String>> {
    let picked = handle.dialog().file().blocking_pick_folder();
    let mut s = app.settings.lock();
    if let Some(p) = picked.and_then(|p| p.into_path().ok()) {
        let p = p.display().to_string();
        if !s.folders.contains(&p) {
            s.folders.push(p);
            s.save();
        }
    }
    Ok(s.folders.clone())
}

#[tauri::command]
async fn library_remove_folder(app: St<'_>, path: String) -> R<Vec<String>> {
    let mut s = app.settings.lock();
    s.folders.retain(|f| *f != path);
    s.save();
    Ok(s.folders.clone())
}

// ---------------------------------------------------------------- project files

#[tauri::command]
async fn project_new(app: St<'_>) -> R<StateDto> {
    app.engine.new_project(Project::empty());
    app.recovery.clear();
    Ok(app.engine.state())
}

/// `project_open`'s reply: the opened project's whole state (the UI replaces its own with it)
/// and the sources that could not be loaded.
fn open_reply(state: StateDto, warnings: Vec<String>) -> Value {
    json!({ "state": state, "warnings": warnings })
}

#[tauri::command]
async fn project_open(app: St<'_>, handle: AppHandle, path: Option<String>) -> R<Value> {
    let path = match path {
        Some(p) => Some(PathBuf::from(p)),
        None => handle.dialog().file().add_filter("WAVE project", &["wave"]).blocking_pick_file().and_then(|p| p.into_path().ok()),
    };
    let Some(path) = path else { return Ok(json!({ "cancelled": true })) };
    let e = app.engine.clone();
    let p2 = path.clone();
    let warnings = tauri::async_runtime::spawn_blocking(move || e.open(&p2)).await.map_err(es)?.map_err(es)?;
    app.recovery.clear();
    let mut s = app.settings.lock();
    s.last_project = Some(path.display().to_string());
    s.save();
    Ok(open_reply(app.engine.state(), warnings))
}

#[tauri::command]
async fn project_save(app: St<'_>, handle: AppHandle, save_as: Option<bool>) -> R<Value> {
    let cur = app.engine.state().path.map(PathBuf::from);
    let path = match (cur, save_as.unwrap_or(false)) {
        (Some(p), false) => Some(p),
        _ => handle.dialog().file().add_filter("WAVE project", &["wave"]).set_file_name("Song.wave").blocking_save_file().and_then(|p| p.into_path().ok()),
    };
    let Some(mut path) = path else { return Ok(json!({ "cancelled": true })) };
    if path.extension().is_none() {
        path.set_extension("wave");
    }
    let e = app.engine.clone();
    let p2 = path.clone();
    tauri::async_runtime::spawn_blocking(move || e.save(&p2)).await.map_err(es)?.map_err(es)?;
    app.recovery.clear();
    let mut s = app.settings.lock();
    s.last_project = Some(path.display().to_string());
    s.save();
    Ok(json!({ "path": path.display().to_string() }))
}

/// Autosave tick for a project that was never saved: writes (or, once it has a file or
/// nothing to lose, removes) the crash-recovery copy. `true` when a copy was written.
#[tauri::command]
async fn recovery_write(app: St<'_>) -> R<bool> {
    let a = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || a.recovery.write(&a.engine)).await.map_err(es)?
}

/// Unsaved projects left behind by a WAVE that crashed or was killed, newest first.
#[tauri::command]
async fn recovery_list(app: St<'_>) -> R<Vec<recovery::Orphan>> {
    Ok(app.recovery.orphans())
}

/// Opens a recovered project as an unsaved project; replies like `project_open`.
#[tauri::command]
async fn recovery_restore(app: St<'_>, id: String) -> R<Value> {
    let a = app.inner().clone();
    let warnings = tauri::async_runtime::spawn_blocking(move || a.recovery.restore(&a.engine, &id)).await.map_err(es)??;
    Ok(open_reply(app.engine.state(), warnings))
}

/// Deletes a recovered project the user does not want back.
#[tauri::command]
async fn recovery_discard(app: St<'_>, id: String) -> R<()> {
    app.recovery.discard(&id)
}

/// Bounce the master mix to a 32-bit float stereo WAV, or a CBR MP3 (`format: "mp3"`,
/// `mp3_kbps` 128/192/256/320, default 320), song or loop + tail.
///
/// Opens a native save dialog first. The render runs off the UI thread so playback
/// keeps going; call `export_cancel` to abort before a file is written.
#[tauri::command]
async fn export_audio(app: St<'_>, handle: AppHandle, use_loop: bool, tail: f64, sample_rate: u32, format: Option<String>, mp3_kbps: Option<u32>) -> R<Value> {
    let fmt = actions::ExportFormat::parse(format.as_deref(), mp3_kbps)?;
    let ext = fmt.extension();
    if !(sample_rate == 44_100 || sample_rate == 48_000) {
        return Err("sample rate must be 44100 or 48000".into());
    }
    if !(tail == 0.0 || tail == 2.0 || tail == 8.0) {
        return Err("tail must be 0, 2 or 8 seconds".into());
    }
    let st = app.engine.state();
    let (from, to) = Engine::export_range(&st.project, use_loop);
    let suggested = st
        .path
        .as_deref()
        .and_then(|p| PathBuf::from(p).file_stem().map(|s| format!("{}.{ext}", s.to_string_lossy())))
        .unwrap_or_else(|| format!("Song.{ext}"));
    let picked = handle
        .dialog()
        .file()
        .add_filter(if ext == "mp3" { "MP3 audio" } else { "WAV audio" }, &[ext])
        .set_file_name(&suggested)
        .blocking_save_file()
        .and_then(|p| p.into_path().ok());
    let Some(mut path) = picked else {
        return Ok(json!({ "cancelled": true }));
    };
    if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case(ext)) != Some(true) {
        path.set_extension(ext);
    }
    app.export_cancel.store(false, Ordering::Relaxed);
    let a = app.inner().clone();
    let out = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        actions::render_to_file(&a, &out, from, to, tail, sample_rate, &a.export_cancel, fmt)
    })
    .await
    .map_err(es)?;
    match result {
        Ok(v) => Ok(v),
        Err(e) if e == "cancelled" => Ok(json!({ "cancelled": true })),
        Err(e) => Err(e),
    }
}

#[tauri::command]
async fn export_cancel(app: St<'_>) -> R<()> {
    app.export_cancel.store(true, Ordering::Relaxed);
    Ok(())
}

// ---------------------------------------------------------------- plugins

#[tauri::command]
async fn vst3_scan(app: St<'_>, rescan: bool) -> R<Vec<Value>> {
    let p = app.plugins.clone();
    tauri::async_runtime::spawn_blocking(move || p.scan(rescan)).await.map_err(es)
}

#[tauri::command]
async fn vst3_add(app: St<'_>, insert_id: u64, path: String, cid: String, index: Option<usize>) -> R<Value> {
    let (id, rev) = actions::vst3_add(&app, insert_id, &path, &cid, index)?;
    Ok(json!({ "fxId": id, "revision": rev }))
}

#[tauri::command]
async fn vst3_editor(app: St<'_>, handle: AppHandle, fx_id: u64, open: bool) -> R<()> {
    app.plugins.editor(&handle, fx_id, open)
}

#[tauri::command]
async fn vst3_params(app: St<'_>, fx_id: u64) -> R<Vec<plugins::ParamInfo>> {
    app.plugins.params(fx_id)
}

#[tauri::command]
async fn vst3_set_param(app: St<'_>, fx_id: u64, id: u32, value: f64) -> R<()> {
    app.plugins.set_param(fx_id, id, value)
}

#[tauri::command]
async fn app_info(app: St<'_>, handle: AppHandle) -> R<updater::AppInfo> {
    let s = app.settings.lock();
    Ok(updater::AppInfo { version: handle.package_info().version.to_string(), seen_version: s.seen_version.clone(), auto_update: !s.no_auto_update, plugin_editors: cfg!(windows) })
}

#[tauri::command]
async fn update_set_auto(app: St<'_>, on: bool) -> R<()> {
    let mut s = app.settings.lock();
    s.no_auto_update = !on;
    s.save();
    Ok(())
}

/// Remembers that the release notes of `version` were shown.
#[tauri::command]
async fn update_mark_seen(app: St<'_>, version: String) -> R<()> {
    let mut s = app.settings.lock();
    s.seen_version = Some(version);
    s.save();
    Ok(())
}

/// Asks GitHub for a newer release. `None` = this is the newest.
#[tauri::command]
async fn update_check(app: St<'_>, handle: AppHandle) -> R<Option<updater::UpdateInfo>> {
    updater::check(&handle, &app.updater).await
}

/// Downloads and verifies the update found by `update_check` (progress: `update-progress`).
#[tauri::command]
async fn update_download(app: St<'_>, handle: AppHandle) -> R<()> {
    updater::download(&handle, &app.updater).await
}

/// Installs the downloaded update and restarts the app. On Windows the installer takes over
/// and the process ends; it does not return then.
#[tauri::command]
async fn update_install(app: St<'_>, handle: AppHandle) -> R<()> {
    let a = app.inner().clone();
    let h = handle.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let ready = updater::ready(&a.updater)?;
        // the installer may end the process without a normal exit: detach plugin editors and stop audio first
        shutdown(&h, &a);
        ready.update.install(ready.bytes.as_slice()).map_err(es)
    })
    .await
    .map_err(es)?;
    match res {
        Ok(()) => handle.restart(),
        Err(e) => {
            // the old version keeps running: audio comes back
            let a = app.inner().clone();
            let cfg = a.settings.lock().audio.clone();
            let st = tauri::async_runtime::spawn_blocking(move || a.audio.start(cfg)).await.map_err(es)?;
            *app.status.lock() = st.clone();
            let _ = handle.emit("audio-status", &st);
            Err(e)
        }
    }
}

#[tauri::command]
async fn open_release_page(version: Option<String>) -> R<()> {
    updater::open_release(version.as_deref())
}

#[tauri::command]
async fn cli_status(app: St<'_>) -> R<CliStatus> {
    Ok(app.cli.lock().clone())
}

// ---------------------------------------------------------------- boot

/// Plugin editors detach first, then audio stops, then instances go.
fn shutdown(h: &AppHandle, app: &App) {
    if let Some(w) = h.try_state::<wave_plugin_windows::Service>() {
        w.close_all();
    }
    app.audio.stop();
    // a normal exit: the user already answered the unsaved-changes question
    app.recovery.close();
}

/// Keeps `LC_NUMERIC` in the C locale. `gtk_init` runs `setlocale(LC_ALL, "")`,
/// so on comma-decimal locales (de, da, pl, …) every C-side text parse of a
/// VST3 preset — `atof` inside DPF-style state restore — reads `0.4` as `0`,
/// which some plugins turn into division-by-zero NaNs. Hosts keep numeric
/// formatting themselves (the UI formats for the user), so nothing else sees
/// the change. Call once before any plugin work and again after `gtk_init`.
pub fn ensure_c_numeric_locale() {
    // SAFETY: `setlocale` with a valid category and NUL-terminated string; the
    // returned locale string is ignored. `LC_NUMERIC` and "C" are C-ABI constants.
    unsafe extern "C" {
        fn setlocale(category: i32, locale: *const std::ffi::c_char) -> *mut std::ffi::c_char;
    }
    unsafe {
        setlocale(libc::LC_NUMERIC, b"C\0".as_ptr().cast());
    }
}

pub fn run() {
    let plugins = plugins::Plugins::new();
    let settings = settings::Settings::load();
    let project = settings
        .last_project
        .as_ref()
        .filter(|p| std::path::Path::new(p).is_file())
        .cloned();
    let engine = Engine::new(Project::empty(), plugins.factory());
    if let Some(p) = project {
        if let Err(e) = engine.open(std::path::Path::new(&p)) {
            log::warn!("could not reopen {p}: {e}");
        }
    }
    let events: Arc<std::sync::OnceLock<AppHandle>> = Arc::new(std::sync::OnceLock::new());
    // an output the device or driver stopped is reopened by the audio thread: tell the UI
    let audio = {
        let ev = events.clone();
        AudioIo::spawn_watched(engine.clone(), move |st| {
            if let Some(h) = ev.get() {
                let _ = h.emit("audio-status", st);
            }
        })
    };
    let midi = {
        let (e1, e2, ev1, ev2) = (engine.clone(), engine.clone(), events.clone(), events.clone());
        MidiInput::start(
            Arc::new(wave_midi_in::MidirBackend),
            wave_midi_in::Hooks {
                message: Arc::new(move |m| e1.midi_event(m)),
                status: Arc::new(move |st: &MidiStatus| {
                    // a device appearing or vanishing switches live notes on/off (and releases them)
                    e2.midi_configure(st.target, st.connected.is_some());
                    if let Some(h) = ev1.get() {
                        let _ = h.emit("midi-status", st);
                    }
                }),
                note: Arc::new(move |n| {
                    if let Some(h) = ev2.get() {
                        let _ = h.emit("midi-note", n);
                    }
                }),
            },
        )
    };
    let app = Arc::new(App {
        engine: engine.clone(),
        audio,
        midi,
        plugins,
        status: Mutex::new(AudioStatus::default()),
        settings: Mutex::new(settings),
        cli: Mutex::new(cli_shim::install()),
        export_cancel: AtomicBool::new(false),
        model_error: Mutex::new(None),
        session: Mutex::new(None),
        keys: keys::Keys::default(),
        updater: updater::State::default(),
        handle: std::sync::OnceLock::new(),
        recovery: recovery::Recovery::start(recovery::default_dir()),
    });
    // the pitch model loads in the background; analysis uses YIN until it is ready
    let a = app.clone();
    std::thread::spawn(move || {
        if let Err(e) = models::activate(&a.engine) {
            log::warn!("{e}");
            *a.model_error.lock() = Some(e);
        }
    });

    // live session for the CLI
    let weak = Arc::downgrade(&app);
    let handler: Arc<wave_session::Handler> = Arc::new(move |req: &wave_session::Request| {
        let Some(app) = weak.upgrade() else { return Err(wave_session::ErrorBody::new("GONE", "the app is closing")) };
        let res = actions::control(&app, &req.command, &req.args, req.if_revision);
        let rev = app.engine.revision();
        res.map(|d| (d, rev)).map_err(|m| {
            let code = m.split(':').next().filter(|c| c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_') && c.len() > 2).unwrap_or("REFUSED").to_string();
            wave_session::ErrorBody::new(&code, m)
        })
    });
    match wave_session::Server::start(handler) {
        Ok(s) => *app.session.lock() = Some(s),
        Err(e) => log::warn!("CLI session unavailable: {e}"),
    }

    let app_setup = app.clone();
    let app_exit = app.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(app.clone())
        .setup(move |tapp| {
            // gtk_init pulled the user locale back over LC_NUMERIC; plugin state
            // text parsing must stay in the C locale (see ensure_c_numeric_locale)
            ensure_c_numeric_locale();
            // plugin work that must happen on the UI thread is posted here
            let mh = tapp.handle().clone();
            wave_vst3::set_main_thread_executor(move |job| {
                let _ = mh.run_on_main_thread(job);
            });
            tapp.manage(wave_plugin_windows::Service::new());
            // open the audio device off the UI thread
            let a = app_setup.clone();
            std::thread::spawn(move || {
                let cfg = a.settings.lock().audio.clone();
                let st = a.audio.start(cfg);
                *a.status.lock() = st;
            });
            let _ = events.set(tapp.handle().clone());
            let _ = app_setup.handle.set(tapp.handle().clone());
            keys::ensure(&app_setup, app_setup.engine.uses_keys());
            // reopen the saved keyboard (it may be unplugged: the status says so, and it opens when it returns)
            let a = app_setup.clone();
            std::thread::spawn(move || {
                let saved = a.settings.lock().midi_device.clone();
                if let Some(d) = saved {
                    a.midi.select(Some(&d.id), Some(&d.name));
                }
            });
            let h = tapp.handle().clone();
            let a = app_setup.clone();
            engine.on_change(move |dto| {
                keys::ensure(&a, dto.project.channels.iter().any(|c| matches!(c.src, wave_engine::ChannelSrc::Keys { .. })));
                let _ = h.emit("state", dto);
            });
            if let Some(w) = tapp.get_webview_window("main") {
                let _ = w.set_title("WAVE");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state, apply_ops, begin_gesture, end_gesture, undo, redo, fx_catalog,
            transport_play, transport_stop, transport_set, telemetry_subscribe, watch_fx,
            import_audio, source_peaks, clip_peaks, pitch_analyze, pitch_target, f0_model_status, f0_model_install, music_analyze, auto_vocal_plan, bake_rider, trim_clip, flatten_comp, comp_audition, preview_clip, preview_file, preview_stop, note_audition, typing_note, pattern_play,
            audio_hosts, audio_devices, audio_get_config, audio_set_config, audio_measure_latency,
            midi_list_devices, midi_select_device, midi_status, midi_set_target,
            library_list, library_install, keys_status, library_files, library_add_folder, library_remove_folder,
            project_new, project_open, project_save, recovery_write, recovery_list, recovery_restore, recovery_discard, export_audio, export_cancel,
            vst3_scan, vst3_add, vst3_editor, vst3_params, vst3_set_param, cli_status,
            app_info, update_set_auto, update_mark_seen, update_check, update_download, update_install, open_release_page
        ])
        .build(tauri::generate_context!())
        .expect("error while building WAVE")
        .run(move |h, ev| {
            if let tauri::RunEvent::Exit = ev {
                shutdown(h, &app_exit);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::open_reply;
    use std::sync::Arc;
    use wave_engine::{Engine, NoPlugins, Project};

    /// The UI replaces its whole state with `state` after an open: it must be a full StateDto
    /// (a reply with only `warnings` crashed the UI on every open).
    #[test]
    fn open_reply_carries_the_opened_state() {
        let dir = std::env::temp_dir().join(format!("wave-open-reply-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Song.wave");
        Engine::new(Project::sketch(), Arc::new(NoPlugins)).save(&path).unwrap();

        let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
        let warnings = e.open(&path).unwrap();
        let v = open_reply(e.state(), warnings);
        std::fs::remove_dir_all(&dir).ok();

        let state = &v["state"];
        assert!(state["project"]["clips"].as_array().is_some_and(|c| !c.is_empty()), "{v}");
        assert!(state["revision"].is_u64());
        assert_eq!(state["dirty"], false);
        assert!(state["path"].as_str().is_some_and(|p| p.ends_with("Song.wave")));
        assert!(v["warnings"].is_array());
        assert!(v.get("cancelled").is_none());
    }
}

mod locale_tests {
    use super::ensure_c_numeric_locale;

    /// `gtk_init` pulls `LC_NUMERIC` to the user's locale; plugin preset text
    /// (`atof` in state restore) must keep parsing `0.4` as 0.4 regardless.
    /// Skips silently when no comma-decimal locale is installed.
    #[test]
    fn numeric_parsing_survives_the_user_locale() {
        // SAFETY: setlocale/strtod with valid NUL-terminated strings; the
        // process is single-threaded here so no other thread reads the locale.
        unsafe extern "C" {
            fn setlocale(category: i32, locale: *const std::ffi::c_char) -> *mut std::ffi::c_char;
            fn strtod(s: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char) -> f64;
        }
        let parse = |s: &str| unsafe {
            let c = std::ffi::CString::new(s).unwrap();
            strtod(c.as_ptr(), std::ptr::null_mut())
        };

        // find an installed comma-decimal locale; the machine's own locale set
        // decides, the test only needs one
        let mut found = None;
        for loc in ["de_DE.UTF-8", "da_DK.UTF-8", "fr_FR.UTF-8", "pl_PL.UTF-8"] {
            let c = std::ffi::CString::new(loc).unwrap();
            let set = unsafe { setlocale(libc::LC_NUMERIC, c.as_ptr()) };
            if !set.is_null() && parse("0.4") != 0.4 {
                found = Some(loc);
                break;
            }
        }
        let Some(loc) = found else {
            return; // no comma-decimal locale installed: nothing to guard against
        };

        ensure_c_numeric_locale();
        assert_eq!(parse("0.4"), 0.4, "after ensure_c_numeric_locale, {loc} must not break decimal parsing");
    }
}
