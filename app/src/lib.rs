//! WAVE desktop app: Tauri commands around the engine, the audio device, the
//! sample library, VST3 plugins and the live session the `wave` CLI talks to.

mod actions;
mod cli_shim;
mod library;
mod plugins;
mod settings;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody, Response};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use wave_audio_io::{AudioConfig, AudioIo, AudioStatus};
use wave_engine::{Engine, Op, Project, StateDto};

pub use cli_shim::CliStatus;

pub struct App {
    pub engine: Arc<Engine>,
    pub audio: AudioIo,
    pub plugins: Arc<plugins::Plugins>,
    pub settings: Mutex<settings::Settings>,
    pub status: Mutex<AudioStatus>,
    pub cli: Mutex<CliStatus>,
    /// Set while an offline bounce is running; flipped by `export_cancel`.
    pub export_cancel: AtomicBool,
    session: Mutex<Option<wave_session::Server>>,
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
    // drop to the low-quality headset profile whenever a mic stream is open). Reopening
    // the devices restarts the audio stream, so closing it waits for the transport to stop.
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
async fn music_analyze(app: St<'_>, clip_id: Option<u64>, exclude_fx: Option<u64>) -> R<wave_engine::music::MusicAnalysis> {
    let e = app.engine.clone();
    tauri::async_runtime::spawn_blocking(move || e.music_analysis(clip_id, exclude_fx)).await.map_err(es)?.map_err(es)
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
async fn audio_set_config(app: St<'_>, handle: AppHandle, config: AudioConfig) -> R<AudioStatus> {
    let a = app.inner().clone();
    let st = tauri::async_runtime::spawn_blocking(move || {
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

// ---------------------------------------------------------------- library

#[tauri::command]
async fn library_list(app: St<'_>) -> R<Value> {
    Ok(json!({ "kits": library::kits(), "folders": app.settings.lock().folders }))
}

#[tauri::command]
async fn library_install(handle: AppHandle, id: String) -> R<()> {
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
    }
    res
}

#[tauri::command]
async fn library_files(kit: Option<String>, folder: Option<String>) -> R<Vec<library::FileInfo>> {
    tauri::async_runtime::spawn_blocking(move || match (kit, folder) {
        (Some(k), _) => Ok(library::kit_files(&k)),
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
    Ok(app.engine.state())
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
    let mut s = app.settings.lock();
    s.last_project = Some(path.display().to_string());
    s.save();
    Ok(json!({ "warnings": warnings }))
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
    let mut s = app.settings.lock();
    s.last_project = Some(path.display().to_string());
    s.save();
    Ok(json!({ "path": path.display().to_string() }))
}

/// Bounce the master mix to a 32-bit float stereo WAV (song or loop + tail).
///
/// Opens a native save dialog first. The render runs off the UI thread so playback
/// keeps going; call `export_cancel` to abort before a file is written.
#[tauri::command]
async fn export_audio(app: St<'_>, handle: AppHandle, use_loop: bool, tail: f64, sample_rate: u32) -> R<Value> {
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
        .and_then(|p| PathBuf::from(p).file_stem().map(|s| format!("{}.wav", s.to_string_lossy())))
        .unwrap_or_else(|| "Song.wav".into());
    let picked = handle
        .dialog()
        .file()
        .add_filter("WAV audio", &["wav"])
        .set_file_name(&suggested)
        .blocking_save_file()
        .and_then(|p| p.into_path().ok());
    let Some(mut path) = picked else {
        return Ok(json!({ "cancelled": true }));
    };
    if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("wav")) != Some(true) {
        path.set_extension("wav");
    }
    app.export_cancel.store(false, Ordering::Relaxed);
    let a = app.inner().clone();
    let out = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        actions::render_to_file(&a, &out, from, to, tail, sample_rate, &a.export_cancel)
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
async fn cli_status(app: St<'_>) -> R<CliStatus> {
    Ok(app.cli.lock().clone())
}

// ---------------------------------------------------------------- boot

pub fn run() {
    let plugins = plugins::Plugins::new();
    let settings = settings::Settings::load();
    let project = settings
        .last_project
        .as_ref()
        .filter(|p| std::path::Path::new(p).is_file())
        .cloned();
    let engine = Engine::new(Project::demo(), plugins.factory());
    if let Some(p) = project {
        if let Err(e) = engine.open(std::path::Path::new(&p)) {
            log::warn!("could not reopen {p}: {e}");
        }
    }
    let audio = AudioIo::spawn(engine.clone());
    let app = Arc::new(App {
        engine: engine.clone(),
        audio,
        plugins,
        status: Mutex::new(AudioStatus::default()),
        settings: Mutex::new(settings),
        cli: Mutex::new(cli_shim::install()),
        export_cancel: AtomicBool::new(false),
        session: Mutex::new(None),
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
        .manage(app.clone())
        .setup(move |tapp| {
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
            let h = tapp.handle().clone();
            engine.on_change(move |dto| {
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
            import_audio, source_peaks, clip_peaks, pitch_analyze, music_analyze, trim_clip, flatten_comp, comp_audition, preview_clip, preview_file, preview_stop, note_audition, pattern_play,
            audio_hosts, audio_devices, audio_get_config, audio_set_config,
            library_list, library_install, library_files, library_add_folder, library_remove_folder,
            project_new, project_open, project_save, export_audio, export_cancel,
            vst3_scan, vst3_add, vst3_editor, vst3_params, vst3_set_param, cli_status
        ])
        .build(tauri::generate_context!())
        .expect("error while building WAVE")
        .run(move |h, ev| {
            if let tauri::RunEvent::Exit = ev {
                // plugin editors detach first, then audio stops, then instances go
                if let Some(w) = h.try_state::<wave_plugin_windows::Service>() {
                    w.close_all();
                }
                app_exit.audio.stop();
            }
        });
}
