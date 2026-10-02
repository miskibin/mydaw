//! Snapshot workers: plugin lifecycle stays on each worker's main thread.
use crate::{AppState, Hosts, ProjectFile, Session};
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderOptions {
    /// pattern or song; uses a snapshot of the current unsaved session.
    pub mode: String,
    pub pattern_id: Option<u64>,
    /// Zero-based bar; preceding bars are rendered to warm voices and effects.
    pub start_bar: Option<u32>,
    pub bars: Option<u32>,
    /// Stop note scheduling and keep processing releases and effects, 0..30 seconds.
    pub tail_seconds: Option<f64>,
    /// 44100, 48000 or 96000 Hz.
    pub sample_rate: Option<u32>,
    /// Solo a stable track ID through its routing and master effects.
    pub track_id: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiagnosticOptions {
    options: RenderOptions,
    track_ids: Vec<u32>,
}
#[derive(Serialize, Deserialize)]
struct WorkerInput {
    #[serde(default)]
    assets_root: Option<PathBuf>,
    project: Option<ProjectFile>,
    render: Option<RenderOptions>,
    input_path: Option<PathBuf>,
    images: bool,
    #[serde(default)]
    diagnostic: Option<DiagnosticOptions>,
}
struct Job {
    status: Value,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<std::process::Child>>,
}
#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<BTreeMap<String, Job>>,
}
impl Jobs {
    pub fn status(&self, id: &str) -> Result<Value, String> {
        self.jobs
            .lock()
            .get(id)
            .map(|j| j.status.clone())
            .ok_or("job is not available in this session".into())
    }
    pub fn cancel(&self, id: &str) -> Result<Value, String> {
        let jobs = self.jobs.lock();
        let job = jobs.get(id).ok_or("job is not available in this session")?;
        job.cancel.store(true, Ordering::Release);
        let _ = job.child.lock().kill();
        Ok(job.status.clone())
    }
    pub fn stop(&self) {
        for job in self.jobs.lock().values() {
            job.cancel.store(true, Ordering::Release);
            // Terminate our own handle now; application exit need not wait for
            // the monitoring thread's next tick before ending this worker.
            let _ = job.child.lock().kill();
        }
    }
    pub fn render(
        self: &Arc<Self>,
        state: &AppState,
        options: RenderOptions,
        revision: u64,
    ) -> Result<Value, String> {
        validate_options(&options)?;
        let project = snapshot(state)?;
        self.start(
            WorkerInput {
                assets_root: state.sampler().root().parent().map(Path::to_path_buf),
                project: Some(project),
                render: Some(options),
                input_path: None,
                images: true,
                diagnostic: None,
            },
            revision,
        )
    }
    pub fn diagnose(
        self: &Arc<Self>,
        state: &AppState,
        options: RenderOptions,
        track_ids: Vec<u32>,
        images: bool,
        revision: u64,
    ) -> Result<Value, String> {
        let project = snapshot(state)?;
        let diagnostic = diagnostic_options(&project, options, track_ids)?;
        self.start(
            WorkerInput {
                assets_root: state.sampler().root().parent().map(Path::to_path_buf),
                project: Some(project),
                render: None,
                input_path: None,
                images,
                diagnostic: Some(diagnostic),
            },
            revision,
        )
    }
    pub fn analyze(
        self: &Arc<Self>,
        path: PathBuf,
        images: bool,
        revision: u64,
    ) -> Result<Value, String> {
        if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
            return Err("analysis input exceeds 256 MiB".into());
        }
        self.start(
            WorkerInput {
                assets_root: None,
                project: None,
                render: None,
                input_path: Some(path),
                images,
                diagnostic: None,
            },
            revision,
        )
    }
    fn start(self: &Arc<Self>, input: WorkerInput, revision: u64) -> Result<Value, String> {
        let mut jobs = self.jobs.lock();
        if jobs
            .values()
            .filter(|j| j.status["status"] == "running")
            .count()
            >= 2
        {
            return Err("two snapshot workers are already running".into());
        }
        while jobs.len() >= 16 {
            let Some(id) = jobs
                .iter()
                .find(|(_, j)| j.status["status"] != "running")
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            jobs.remove(&id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let dir = mydaw_control_protocol::session_dir()
            .parent()
            .unwrap_or(Path::new("."))
            .join("jobs")
            .join(&id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let input_path = dir.join("input.json");
        std::fs::write(
            &input_path,
            serde_json::to_vec(&input).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let output = dir.join("result.json");
        let mut command =
            std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        command
            .env("MYDAW_RENDER_WORKER", &input_path)
            .env_remove(crate::DEBUG_CAPTURE_ENV)
            .env_remove(crate::IPC_BENCH_ENV)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(dir.join("worker.log")).map_err(|e| e.to_string())?);
        if let Some(root) = input.assets_root.as_ref() {
            command.env(mydaw_plugin_sampler::ASSETS_DIR_ENV, root);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let child = Arc::new(Mutex::new(command.spawn().map_err(|e| e.to_string())?));
        let cancel = Arc::new(AtomicBool::new(false));
        let status =
            json!({"jobId":id,"status":"running","snapshotRevision":revision,"directory":dir});
        jobs.insert(
            id.clone(),
            Job {
                status: status.clone(),
                cancel: Arc::clone(&cancel),
                child: Arc::clone(&child),
            },
        );
        drop(jobs);
        let registry = Arc::clone(self);
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(180);
            let outcome = loop {
                if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                    let _ = child.lock().kill();
                    let _ = child.lock().wait();
                    break json!({"status":if cancel.load(Ordering::Acquire){"cancelled"}else{"failed"},"error":"worker cancelled or exceeded 180 seconds"});
                }
                let exit = child.lock().try_wait();
                match exit {
                    Ok(Some(exit)) => {
                        let result = std::fs::read(&output)
                            .ok()
                            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                        if !exit.success()
                            && result.as_ref().is_some_and(|r| r["status"] == "completed")
                        {
                            break json!({"status":"failed","error":format!("worker exited {exit}; see worker.log")});
                        }
                        break result.unwrap_or_else(||json!({"status":"failed","error":format!("worker exited {exit}; see worker.log")}));
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                    Err(error) => {
                        let _ = child.lock().kill();
                        let _ = child.lock().wait();
                        break json!({"status":"failed","error":error.to_string()});
                    }
                }
            };
            if let Some(job) = registry.jobs.lock().get_mut(&id) {
                for (key, value) in outcome.as_object().into_iter().flatten() {
                    job.status[key] = value.clone();
                }
            }
        });
        Ok(status)
    }
}
fn snapshot(state: &AppState) -> Result<ProjectFile, String> {
    let live = state.tracks().project();
    for (index, track) in live.tracks.iter().enumerate() {
        if (track.source.is_clap() && state.clap_bridge().board().track(index).is_none())
            || (track.source.is_vst3() && state.vst3_bridge().board().track(index).is_none())
        {
            return Err(format!("track {} has no live plugin state to snapshot; load its plugin and audio engine first", track.id));
        }
    }
    if state.project_bridge().missing().master().is_some()
        && state.clap_bridge().board().master().is_none()
        && state.vst3_bridge().board().master().is_none()
    {
        return Err("the master plugin is missing; restore or replace it before rendering".into());
    }
    match state
        .project_bridge()
        .request(crate::ProjectCommand::Snapshot)?
    {
        crate::ProjectAnswer::Snapshot(file) => Ok(*file),
        _ => Err("snapshot was not returned".into()),
    }
}

fn diagnostic_options(
    project: &ProjectFile,
    mut options: RenderOptions,
    track_ids: Vec<u32>,
) -> Result<DiagnosticOptions, String> {
    options.bars = Some(options.bars.unwrap_or(2));
    options.tail_seconds = Some(options.tail_seconds.unwrap_or(0.5));
    validate_options(&options)?;
    if options.track_id.is_some()
        || options.bars.unwrap() > 8
        || options.tail_seconds.unwrap() > 3.0
    {
        return Err(
            "diagnose accepts 1..8 bars, tailSeconds 0..3 and trackIds instead of options.trackId"
                .into(),
        );
    }
    if track_ids.is_empty() || track_ids.len() > 4 {
        return Err("diagnose requires 1..4 stable trackIds".into());
    }
    let mut unique = std::collections::BTreeSet::new();
    for id in &track_ids {
        if !unique.insert(*id) || !project.tracks.iter().any(|t| t.id == *id) {
            return Err(format!("trackId {id} is duplicated or does not exist"));
        }
    }
    Ok(DiagnosticOptions { options, track_ids })
}
fn validate_options(options: &RenderOptions) -> Result<(), String> {
    if !matches!(options.mode.as_str(), "pattern" | "song") {
        return Err("mode must be pattern or song".into());
    }
    if !matches!(options.sample_rate.unwrap_or(48000), 44100 | 48000 | 96000) {
        return Err("sampleRate must be 44100, 48000 or 96000".into());
    }
    if options.bars.is_some_and(|b| b == 0 || b > 512) || options.start_bar.is_some_and(|b| b > 512)
    {
        return Err("bars must be 1..512 and startBar 0..512".into());
    }
    let tail = options.tail_seconds.unwrap_or(2.0);
    if !tail.is_finite() || !(0.0..=30.0).contains(&tail) {
        return Err("tailSeconds must be 0..30".into());
    }
    Ok(())
}
pub fn run_worker(path: &Path) -> std::process::ExitCode {
    let dir = path.parent().unwrap_or(Path::new("."));
    let result = (|| -> Result<Value, String> {
        let input: WorkerInput =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if let Some(diagnostic) = input.diagnostic {
            return diagnose_snapshot(
                &input.project.ok_or("missing diagnostic snapshot")?,
                diagnostic,
                dir,
                input.images,
            );
        }
        let capture = if let (Some(project), Some(options)) = (input.project, input.render) {
            render_snapshot(&project, &options)?
        } else {
            analyze_file(&input.input_path.ok_or("missing analysis file")?)?
        };
        let wav = dir.join("audio.wav");
        capture.write_wav(&wav).map_err(|e| e.to_string())?;
        let stats = capture.stats();
        let stats_path = dir.join("analysis.json");
        std::fs::write(
            &stats_path,
            serde_json::to_vec_pretty(&stats).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let image = if input.images {
            let path = dir.join("waveform-spectrum.png");
            capture.render_png(&path).map_err(|e| e.to_string())?;
            Some(path)
        } else {
            None
        };
        Ok(
            json!({"status":"completed","audio":wav,"analysis":stats_path,"image":image,"sampleRate":capture.sample_rate,"frames":capture.frames(),"seconds":capture.duration_seconds(),
            "summary":stats.per_channel,"diagnostics":crate::mix_diagnostics::measure(&capture),"envelope":capture.sparkline(64)}),
        )
    })();
    let failed = result.is_err();
    let value = result.unwrap_or_else(|e| json!({"status":"failed","error":e}));
    if let Err(error) = std::fs::write(
        dir.join("result.json"),
        serde_json::to_vec(&value).unwrap_or_default(),
    ) {
        eprintln!("worker result: {error}");
        return std::process::ExitCode::FAILURE;
    }
    if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

fn without_master(file: &ProjectFile) -> ProjectFile {
    let mut file = file.clone();
    file.master_gain_db = 0.0;
    file.master_inserts.clear();
    file.master_effect = None;
    file
}

fn diagnostic_artifact(
    file: &ProjectFile,
    options: &RenderOptions,
    dir: &Path,
    name: &str,
    images: bool,
    registry: &Arc<mydaw_plugin_sampler::SamplerRegistry>,
) -> Result<Value, String> {
    let capture = render_snapshot_with_registry(file, options, Some(Arc::clone(registry)))?;
    let audio = dir.join(format!("{name}.wav"));
    capture.write_wav(&audio).map_err(|e| e.to_string())?;
    let metrics = crate::mix_diagnostics::measure(&capture);
    let analysis = dir.join(format!("{name}.analysis.json"));
    std::fs::write(
        &analysis,
        serde_json::to_vec_pretty(&json!({"metrics":metrics,"channels":capture.stats()}))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let image = if images {
        let path = dir.join(format!("{name}.png"));
        capture.render_png(&path).map_err(|e| e.to_string())?;
        Some(path)
    } else {
        None
    };
    Ok(
        json!({"audio":audio,"analysis":analysis,"image":image,"seconds":capture.duration_seconds(),"metrics":metrics}),
    )
}

fn diagnose_snapshot(
    file: &ProjectFile,
    diagnostic: DiagnosticOptions,
    dir: &Path,
    images: bool,
) -> Result<Value, String> {
    let diagnostic = diagnostic_options(file, diagnostic.options, diagnostic.track_ids)?;
    // Per-job cache: IDs are fixed by one snapshot, only immutable decoded
    // banks are reused, no voices/effect state survives a stage, and everything
    // is freed with this worker. A subsequent job decodes current asset files.
    let registry = Arc::new(mydaw_plugin_sampler::SamplerRegistry::new(
        mydaw_plugin_sampler::instruments_root(),
    ));
    let premaster_file = without_master(file);
    let mix = diagnostic_artifact(file, &diagnostic.options, dir, "mix", images, &registry)?;
    let premaster = diagnostic_artifact(
        &premaster_file,
        &diagnostic.options,
        dir,
        "premaster",
        images,
        &registry,
    )?;
    let mut tracks = Vec::new();
    for id in &diagnostic.track_ids {
        let index = file
            .tracks
            .iter()
            .position(|t| t.id == *id)
            .ok_or("track is unavailable")?;
        let track = &file.tracks[index];
        let mut options = diagnostic.options.clone();
        options.track_id = Some(*id);
        let processed = diagnostic_artifact(
            &premaster_file,
            &options,
            dir,
            &format!("track-{id}"),
            images,
            &registry,
        )?;
        let mut dry = premaster_file.clone();
        dry.sends.clear();
        dry.buses.clear();
        for track in &mut dry.tracks {
            track.inserts.clear();
        }
        dry.tracks[index].gain_db = 0.0;
        dry.tracks[index].pan = 0.0;
        dry.tracks[index].muted = false;
        let source = diagnostic_artifact(
            &dry,
            &options,
            dir,
            &format!("source-{id}"),
            images,
            &registry,
        )?;
        tracks.push(
            json!({"trackId":id,"name":track.name,"gainDb":track.gain_db,"muted":track.muted,
            "sourceDescription":track.source,"source":source,"track":processed}),
        );
    }
    let report = json!({"version":1,"options":diagnostic.options,"tracks":tracks,"premaster":premaster,"mix":mix,
        "limitations":["Metrics are evidence, not an audible distortion or mix-quality verdict.",
            "Source renders preserve velocity and instrument patch; they are not raw sample files.",
            "Isolated tracks change sidechain/nonlinear bus context; they do not sum to the mastered mix.",
            "Native LUFS and true peak are unavailable; use a standards-based meter on final exports.",
            "Existing renderer has no plugin delay compensation or sample-accurate plugin automation."]});
    let path = dir.join("diagnostic-report.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({"status":"completed","report":path,"diagnostics":report}))
}

fn analyze_file(path: &Path) -> Result<mydaw_plugin_debug_tap::Capture, String> {
    let audio =
        mydaw_audio_file::decode_stereo(path, |rate| Some((rate.min(192000.0) * 180.0) as usize))
            .map_err(|e| e.to_string())?;
    if audio.truncated || audio.sample_rate > 192000.0 {
        return Err(
            "analysis supports up to 180 seconds at up to 192 kHz; use a shorter excerpt".into(),
        );
    }
    Ok(mydaw_plugin_debug_tap::Capture::new(
        audio.data,
        audio.sample_rate,
        0,
    ))
}

/// Independent services, full graph and both real plugin hosts; no sound card or GUI.
pub fn render_snapshot(
    file: &ProjectFile,
    options: &RenderOptions,
) -> Result<mydaw_plugin_debug_tap::Capture, String> {
    render_snapshot_with_registry(file, options, None)
}

fn render_snapshot_with_registry(
    file: &ProjectFile,
    options: &RenderOptions,
    registry: Option<Arc<mydaw_plugin_sampler::SamplerRegistry>>,
) -> Result<mydaw_plugin_debug_tap::Capture, String> {
    use mydaw_plugin_audio_io::{run_block, AudioIoStats, RtState, RtStateParams};
    use mydaw_plugin_sequencer::{song_channel, step_len_samples, PlayMode};
    use mydaw_rt_core::{
        graph_channel, note_channel, rt_channel, GraphLimits, RtCommand, TransportClock,
    };
    validate_options(options)?;
    file.validate().map_err(|e| e.to_string())?;
    let cached = registry.is_some();
    let mut kernel = match registry {
        Some(registry) => crate::state::boot_render_kernel_with_registry(registry),
        None => crate::state::boot_render_kernel(),
    }
    .map_err(|e| e.to_string())?;
    let state = AppState::from_kernel(&kernel).map_err(|e| e.to_string())?;
    // Normal GUI boot discovers banks asynchronously. A worker cannot race that
    // scan or silently render missing instruments: load its required banks now.
    for track in &file.tracks {
        if let crate::SourceEntry::Internal { id } = &track.source {
            if let Some(instrument) = id.strip_prefix("sampler:") {
                if !instrument.starts_with("file:") && !state.sampler().contains(id) {
                    state.sampler().install_instrument(instrument)?;
                } else if cached && !state.sampler().contains(id) {
                    if let Some(path) = mydaw_plugin_sampler::file_path_of(instrument) {
                        state
                            .sampler()
                            .install_file(path)
                            .map_err(|e| e.to_string())?;
                    }
                }
                if cached
                    && state
                        .sampler()
                        .instruments()
                        .iter()
                        .map(|bank| bank.bytes)
                        .fold(0usize, usize::saturating_add)
                        > 512 * 1024 * 1024
                {
                    return Err(
                        "diagnostic decoded sample banks exceed the 512 MiB per-job cache limit"
                            .into(),
                    );
                }
            }
        }
    }
    let rate = f64::from(options.sample_rate.unwrap_or(48000));
    let max_block = 512;
    let mut gc = mydaw_rt_core::basedrop::Collector::new();
    let windows = Arc::new(mydaw_plugin_windows::PluginWindowService::new());
    let mut clap = crate::ClapHost::new(Arc::clone(&windows));
    let mut vst3 = crate::Vst3Host::new(windows);
    let session = Session {
        tracks: state.tracks(),
        audio: state.audio(),
        sequencer: state.sequencer(),
        sources: state.audio_sources(),
    };
    let mut hosts = Hosts {
        clap: &mut clap,
        clap_board: state.clap_bridge().board(),
        vst3: &mut vst3,
        vst3_board: state.vst3_bridge().board(),
    };
    let mut report = crate::ProjectHost::new()
        .prepare(&session, &mut hosts, file, None)
        .map_err(|e| e.to_string())?;
    if let Some(id) = options.pattern_id {
        state
            .sequencer()
            .edit_patterns(|patterns| patterns.set_active(id))
            .map_err(|e| e.to_string())?;
    }
    if let Some(id) = options.track_id {
        let project = state.tracks().project();
        let index = project
            .tracks
            .iter()
            .position(|t| t.id == id)
            .ok_or("trackId does not exist")?;
        for track in 0..project.tracks.len() {
            state
                .tracks()
                .set_soloed(track, track == index)
                .map_err(|e| e.to_string())?;
        }
    }
    state.tracks().ensure_file_samplers();
    for track in &state.tracks().project().tracks {
        if let crate::Source::Sampler(reference) = &track.source {
            if !state.sampler().contains(&reference.wire_id()) {
                return Err(format!(
                    "sampled instrument {} could not be loaded",
                    reference.instrument_id
                ));
            }
        }
    }
    let (mut graph, slot) = graph_channel(2, gc.handle());
    graph
        .install(state.tracks().build_spec(
            &GraphLimits {
                sample_rate: rate,
                max_block,
                max_events: mydaw_rt_core::DEFAULT_NOTE_CAPACITY,
            },
            gc.handle(),
        ))
        .map_err(|e| e.to_string())?;
    crate::attach(&session, &mut hosts, file, rate, max_block, &mut report);
    if !report.warnings.is_empty()
        || report.missing_tracks.iter().any(Option::is_some)
        || report.missing_master.is_some()
    {
        drop(slot);
        drop(graph);
        gc.collect();
        hosts.vst3.shutdown();
        hosts.clap.shutdown();
        kernel.shutdown();
        return Err(format!(
            "snapshot cannot be rendered completely: {report:?}"
        ));
    }
    let project = state.tracks().project();
    let clip_install = state.tracks().audio_clips().install(
        state
            .sequencer()
            .compile_audio_with(state.audio_sources(), |id| project.audio_clip_tune(id)),
    );
    // Instrument-only graphs have no audio-clip consumer. install still caches
    // the arrangement, so a missing consumer is expected in that case.
    if let Err(error) = clip_install {
        if !matches!(
            error,
            crate::SourceError::Audio(mydaw_plugin_audio_io::AudioIoError::NotRunning)
        ) {
            return Err(error.to_string());
        }
    }
    let pattern = state.sequencer().active_pattern();
    let bar_frames = step_len_samples(rate, file.bpm) * 16.0;
    let bars = options.bars.unwrap_or_else(|| {
        if options.mode == "pattern" {
            u32::from(pattern.length_steps) / 16
        } else {
            state
                .sequencer()
                .playlist()
                .end_ticks()
                .div_ceil(3840)
                .max(1) as u32
        }
    });
    let start = (f64::from(options.start_bar.unwrap_or(0)) * bar_frames).round() as usize;
    let body = (f64::from(bars) * bar_frames).round() as usize;
    let tail = (options.tail_seconds.unwrap_or(2.0) * rate).round() as usize;
    if (start + body + tail) as f64 / rate > 180.0 {
        return Err("snapshot render exceeds 180 seconds of audio; render a shorter range".into());
    }
    let (mut commands, command_rx) = rt_channel::<RtCommand>(8);
    let (_notes_tx, notes) = note_channel(mydaw_rt_core::DEFAULT_NOTE_CAPACITY);
    let (mut song_tx, song_slot) = song_channel(2, gc.handle());
    song_tx
        .install(state.sequencer().compile())
        .map_err(|e| e.to_string())?;
    for command in [
        RtCommand::SetBpm(file.bpm),
        RtCommand::SetPlayMode(if options.mode == "pattern" {
            PlayMode::Pattern
        } else {
            PlayMode::Song
        }),
        RtCommand::SetLooping(false),
        RtCommand::SetPlaying(true),
    ] {
        commands.push(command).map_err(|e| e.to_string())?;
    }
    let stats = Arc::new(AudioIoStats::default());
    let mut rt = RtState::new(RtStateParams {
        graph: slot,
        commands: command_rx,
        notes,
        note_capacity: mydaw_rt_core::DEFAULT_NOTE_CAPACITY,
        song: song_slot,
        meters: None,
        debug_tap: None,
        spectrum: None,
        transport: Arc::new(TransportClock::new(rate)),
        sample_rate: rate,
        out_channels: 2,
        max_block,
        stats: Arc::clone(&stats),
    });
    let mut data = Vec::with_capacity((body + tail) * 2);
    let mut block = vec![0.0; 256 * 2];
    let mut position = 0;
    while position < start + body + tail {
        if position == start + body {
            commands
                .push(RtCommand::SetPlaying(false))
                .map_err(|e| e.to_string())?;
        }
        let boundary = if position < start {
            start
        } else if position < start + body {
            start + body
        } else {
            start + body + tail
        };
        let frames = (boundary - position).min(256);
        run_block(&mut rt, &mut block[..frames * 2]);
        if position >= start {
            data.extend_from_slice(&block[..frames * 2]);
        }
        position += frames;
    }
    let dropped = stats.note_events_dropped();
    drop(rt);
    drop(graph);
    gc.collect();
    hosts.vst3.shutdown();
    hosts.clap.shutdown();
    kernel.shutdown();
    if dropped != 0 {
        return Err(format!("render dropped {dropped} note events"));
    }
    if data.iter().any(|v| !v.is_finite()) {
        return Err("plugin produced non-finite audio".into());
    }
    Ok(mydaw_plugin_debug_tap::Capture::new(data, rate, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChannelClipDto, ClapEntry, EffectEntry, EffectRef, MelodicNoteDto, PatternDto, SourceEntry,
    };

    fn file() -> ProjectFile {
        let mut kernel = crate::boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        let mut file = crate::default_project_file(state.sampler());
        file.tracks.truncate(1);
        file.tracks[0].source = SourceEntry::Internal {
            id: "poly-synth".into(),
        };
        file.tracks[0].source_params.clear();
        file.tracks[0].inserts.clear();
        file.bpm = 120.0;
        file.patterns = vec![PatternDto {
            id: 1,
            name: "Melody".into(),
            color: None,
            steps: 16,
            channels: vec![ChannelClipDto {
                channel: 0,
                mode: "roll".into(),
                steps: None,
                notes: vec![MelodicNoteDto {
                    key: 69,
                    start: 0,
                    length: 1920,
                    velocity: 0.8,
                }],
            }],
        }];
        file.active_pattern = 1;
        kernel.shutdown();
        file
    }
    fn options() -> RenderOptions {
        RenderOptions {
            mode: "pattern".into(),
            pattern_id: Some(1),
            start_bar: None,
            bars: Some(1),
            tail_seconds: Some(0.25),
            sample_rate: Some(48000),
            track_id: None,
        }
    }
    fn peak(capture: &mydaw_plugin_debug_tap::Capture) -> f32 {
        capture.samples.iter().map(|v| v.abs()).fold(0.0, f32::max)
    }
    #[test]
    fn renders_an_unsaved_instrument_graph_without_an_audio_device_and_bounds_ranges() {
        let file = file();
        let capture = render_snapshot(&file, &options()).unwrap();
        assert_eq!(capture.frames(), 108000);
        assert!(peak(&capture) > 0.01);
        assert!(capture.samples.iter().all(|v| v.is_finite()));
        let mut bad = options();
        bad.bars = Some(0);
        assert!(render_snapshot(&file, &bad).unwrap_err().contains("bars"));
        bad = options();
        bad.track_id = Some(u32::MAX);
        assert!(render_snapshot(&file, &bad)
            .unwrap_err()
            .contains("trackId"));
    }
    #[test]
    fn real_clap_state_and_master_insert_are_audible_and_missing_plugins_fail() {
        let directory =
            std::env::temp_dir().join(format!("mydaw-cli-render-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let bundle = mydaw_plugin_host_clap::test_util::stage_fixture_bundle(&directory)
            .expect("the workspace fixture must be built");
        let mut file = file();
        let entry = |id: &str| ClapEntry {
            bundle_path: bundle.to_string_lossy().into_owned(),
            plugin_id: id.into(),
            name: id.into(),
        };
        file.tracks[0].source = SourceEntry::Clap(entry(mydaw_fixture_clap_gain::synth::PLUGIN_ID));
        file.tracks[0].state_blob =
            crate::to_base64(&mydaw_fixture_clap_gain::synth::state_blob(12.0));
        let plain = render_snapshot(&file, &options()).unwrap();
        let spectrum = plain.stats();
        assert!((spectrum.per_channel[0].top_bins[0].frequency_hz - 880.0).abs() < 4.0);
        assert!(peak(&plain) > 0.01);
        file.master_effect = Some(EffectEntry {
            plugin: EffectRef::Clap(entry(mydaw_fixture_clap_gain::PLUGIN_ID)),
            state_blob: String::new(),
        });
        let processed = render_snapshot(&file, &options()).unwrap();
        assert!((peak(&processed) / peak(&plain) - 0.5).abs() < 0.001);
        file.tracks[0].source = SourceEntry::Clap(ClapEntry {
            bundle_path: directory
                .join("missing.clap")
                .to_string_lossy()
                .into_owned(),
            ..entry("missing")
        });
        assert!(render_snapshot(&file, &options())
            .unwrap_err()
            .contains("cannot be rendered completely"));
        // clack keeps the loaded bundle cached until process exit on Windows.
        let _ = std::fs::remove_dir_all(directory);
    }
    #[test]
    fn analysis_decodes_mono_pcm_as_stereo() {
        let path =
            std::env::temp_dir().join(format!("mydaw-analysis-{}.wav", uuid::Uuid::new_v4()));
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 44100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        writer.write_sample(16384_i16).unwrap();
        writer.write_sample(-16384_i16).unwrap();
        writer.finalize().unwrap();
        let capture = analyze_file(&path).unwrap();
        assert_eq!(capture.sample_rate, 44100.0);
        assert_eq!(capture.samples, vec![0.5, 0.5, -0.5, -0.5]);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn a_dropped_sample_is_rendered_and_a_missing_bank_is_refused() {
        let path =
            std::env::temp_dir().join(format!("mydaw-render-sample-{}.wav", uuid::Uuid::new_v4()));
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for _ in 0..4800 {
            writer.write_sample(0.25_f32).unwrap();
        }
        writer.finalize().unwrap();
        let mut project = file();
        project.tracks[0].source = SourceEntry::Internal {
            id: mydaw_plugin_sampler::source_id(&mydaw_plugin_sampler::file_instrument_id(&path)),
        };
        let capture = render_snapshot(&project, &options()).unwrap();
        assert!(peak(&capture) > 0.01);
        project.tracks[0].source = SourceEntry::Internal {
            id: "sampler:missing-offline-bank".into(),
        };
        assert!(render_snapshot(&project, &options()).is_err());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn a_missing_master_is_refused_before_snapshotting_an_incomplete_live_graph() {
        let mut kernel = crate::boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        state.project_bridge().missing().publish(
            vec![],
            Some(crate::MissingPluginDto {
                name: "Missing effect".into(),
                format: "clap".into(),
                path: "missing.clap".into(),
                reason: "not installed".into(),
            }),
        );
        assert!(state
            .jobs()
            .render(&state, options(), 0)
            .unwrap_err()
            .contains("master plugin is missing"));
        kernel.shutdown();
    }

    #[test]
    fn diagnostic_bounds_and_ids_fail_before_rendering() {
        let file = file();
        let id = file.tracks[0].id;
        assert!(diagnostic_options(&file, options(), vec![]).is_err());
        assert!(diagnostic_options(&file, options(), vec![id, id]).is_err());
        assert!(diagnostic_options(&file, options(), vec![u32::MAX]).is_err());
        let mut long = options();
        long.bars = Some(9);
        assert!(diagnostic_options(&file, long, vec![id]).is_err());
        let mut ambiguous = options();
        ambiguous.track_id = Some(id);
        assert!(diagnostic_options(&file, ambiguous, vec![id]).is_err());
    }

    #[test]
    fn diagnostic_reuses_decoded_banks_but_restarts_voices_and_new_jobs_reload_files() {
        let mut file = file();
        let directory =
            std::env::temp_dir().join(format!("mydaw-bank-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("tone.wav");
        let source = mydaw_plugin_debug_tap::Capture::new(
            (0..4800)
                .flat_map(|i| {
                    let sample = (std::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() * 0.5;
                    [sample, sample]
                })
                .collect(),
            48000.0,
            0,
        );
        source.write_wav(&path).unwrap();
        let id = format!(
            "sampler:{}",
            mydaw_plugin_sampler::file_instrument_id(&path)
        );
        file.tracks[0].source = SourceEntry::Internal { id: id.clone() };
        let registry = Arc::new(mydaw_plugin_sampler::SamplerRegistry::detached());
        let first =
            render_snapshot_with_registry(&file, &options(), Some(Arc::clone(&registry))).unwrap();
        let bank = registry.bank(&id).unwrap();
        std::fs::remove_file(&path).unwrap();
        let second =
            render_snapshot_with_registry(&file, &options(), Some(Arc::clone(&registry))).unwrap();
        assert_eq!(first.samples, second.samples);
        assert!(Arc::ptr_eq(&bank, &registry.bank(&id).unwrap()));
        assert!(render_snapshot(&file, &options()).is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn diagnostic_stages_expose_upstream_levels_without_changing_snapshot() {
        let mut file = file();
        let id = file.tracks[0].id;
        file.tracks[0].gain_db = -12.0;
        file.master_gain_db = -6.0;
        let before = file.clone();
        let directory =
            std::env::temp_dir().join(format!("mydaw-diagnostic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let options = diagnostic_options(&file, options(), vec![id]).unwrap();
        let result = diagnose_snapshot(&file, options, &directory, false).unwrap();
        assert_eq!(file, before);
        let report = &result["diagnostics"];
        let peak = |stage: &Value| stage["metrics"]["samplePeakDbfs"].as_f64().unwrap();
        let source = &report["tracks"][0]["source"];
        let track = &report["tracks"][0]["track"];
        assert!((peak(source) - peak(track) - 12.0).abs() < 0.02);
        assert!((peak(&report["premaster"]) - peak(&report["mix"]) - 6.0).abs() < 0.02);
        assert!(source["image"].is_null());
        assert!(Path::new(source["audio"].as_str().unwrap()).is_file());
        assert!(Path::new(result["report"].as_str().unwrap()).is_file());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
