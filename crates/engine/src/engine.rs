//! The non-real-time side of the engine: owns the project, undo history and the
//! loaded audio, turns edits into ready-built structures for the audio thread,
//! finalises recordings, and publishes telemetry.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::analysis::Analyzer;
use crate::catalog::param_block;
use crate::model::*;
use crate::ops::{self, Dirty, Op};
use crate::processor::{ParamBlock, Processor};
use crate::rt::*;
use crate::sources::{write_wav, Audio, SourceStore};

/// Creates processors the engine cannot build itself (hosted plugins).
pub trait PluginFactory: Send + Sync {
    /// `live` is false for offline renders: the factory must not replace the live instance's handle.
    fn create(&self, fx_id: Id, plugin: &PluginRef, sample_rate: f32, max_block: usize, live: bool) -> Result<Box<dyn Processor>, String>;
    /// The effect left the project (or the engine is rebuilding everything).
    fn release(&self, _fx_id: Id) {}
    /// Current opaque state of a live instance (base64), for saving.
    fn state(&self, _fx_id: Id) -> Option<String> {
        None
    }
}

pub struct NoPlugins;
impl PluginFactory for NoPlugins {
    fn create(&self, _: Id, p: &PluginRef, _: f32, _: usize, _: bool) -> Result<Box<dyn Processor>, String> {
        Err(format!("{} cannot be loaded: this build has no plugin host", p.name))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TransportDto {
    pub playing: bool,
    pub rec: bool,
    pub monitor: bool,
    pub metro: bool,
    pub count_in: bool,
    pub start_beat: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateDto {
    pub project: Project,
    pub revision: u64,
    pub can_undo: bool,
    pub can_redo: bool,
    pub transport: TransportDto,
    pub path: Option<String>,
    pub dirty: bool,
    /// Effects that failed to load (plugin missing etc.): fx id → message.
    pub fx_errors: HashMap<Id, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Op(#[from] ops::OpError),
    #[error("{0}")]
    Msg(String),
}

impl From<String> for EngineError {
    fn from(s: String) -> Self {
        EngineError::Msg(s)
    }
}

type Listener = Box<dyn Fn(&StateDto) + Send + Sync>;
type FrameSink = Box<dyn Fn(Vec<u8>) + Send + Sync>;

struct Take {
    track: usize,
    start: f64,
    sr: u32,
    data: Vec<f32>,
    peaks: Vec<f32>,
    peaks_sent: usize,
    acc: (f32, f32, usize),
}

struct Wires {
    cmds: rtrb::Producer<Cmd>,
    garbage: rtrb::Consumer<Garbage>,
    record: rtrb::Consumer<f32>,
    analyzer: Analyzer,
    sr: f32,
    max_block: usize,
}

struct Core {
    project: Project,
    undo: Vec<Project>,
    redo: Vec<Project>,
    revision: u64,
    gesture: Option<Project>,
    sources: SourceStore,
    path: Option<PathBuf>,
    saved_revision: u64,
    transport: TransportDto,
    /// Effect ids the audio thread currently holds a processor for.
    live_fx: HashSet<Id>,
    sent_params: HashMap<Id, ParamBlock>,
    fx_errors: HashMap<Id, String>,
    wires: Option<Wires>,
    take: Option<Take>,
    watch: Vec<Id>,
}

pub struct Engine {
    core: Mutex<Core>,
    shared: Mutex<Arc<Shared>>,
    factory: Arc<dyn PluginFactory>,
    listeners: Mutex<Vec<Listener>>,
    sink: Mutex<Option<FrameSink>>,
}

const UNDO_LIMIT: usize = 200;

impl Engine {
    pub fn new(project: Project, factory: Arc<dyn PluginFactory>) -> Arc<Self> {
        let e = Arc::new(Self {
            core: Mutex::new(Core {
                project,
                undo: vec![],
                redo: vec![],
                revision: 1,
                gesture: None,
                sources: SourceStore::default(),
                path: None,
                saved_revision: 1,
                transport: TransportDto { playing: false, rec: false, monitor: false, metro: false, count_in: true, start_beat: 0.0 },
                live_fx: HashSet::new(),
                sent_params: HashMap::new(),
                fx_errors: HashMap::new(),
                wires: None,
                take: None,
                watch: vec![],
            }),
            shared: Mutex::new(Arc::new(Shared::new())),
            factory,
            listeners: Mutex::new(vec![]),
            sink: Mutex::new(None),
        });
        let weak = Arc::downgrade(&e);
        thread::Builder::new()
            .name("wave-housekeeping".into())
            .spawn(move || {
                let mut tick = 0u32;
                while let Some(e) = weak.upgrade() {
                    e.housekeeping(tick);
                    drop(e);
                    tick = tick.wrapping_add(1);
                    thread::sleep(Duration::from_millis(16));
                }
            })
            .expect("spawn housekeeping thread");
        e
    }

    // ------------------------------------------------------------ device attach

    /// Builds the audio-thread half for a device running at `sample_rate` with
    /// blocks of at most `max_block`. Any previous `Rt` becomes inert: drop it
    /// (off the audio thread) once its stream has stopped.
    pub fn create_rt(&self, sample_rate: f32, max_block: usize) -> Rt {
        let max_block = max_block.clamp(16, 8192);
        let (cmd_tx, cmd_rx) = rtrb::RingBuffer::<Cmd>::new(4096);
        let (gb_tx, gb_rx) = rtrb::RingBuffer::<Garbage>::new(4096);
        let (rec_tx, rec_rx) = rtrb::RingBuffer::<f32>::new(sample_rate as usize * 2 * 20);
        let mut inputs = vec![];
        let mut outputs = vec![];
        let mut ain = vec![];
        let mut aout = vec![];
        for _ in 0..WATCH_SLOTS {
            let (a, b) = rtrb::RingBuffer::<f32>::new(TAP_CAPACITY);
            inputs.push(a);
            ain.push(b);
            let (a, b) = rtrb::RingBuffer::<f32>::new(TAP_CAPACITY);
            outputs.push(a);
            aout.push(b);
        }
        let shared = Arc::new(Shared::new());
        *self.shared.lock() = shared.clone();
        let rt = Rt::new(sample_rate, max_block, cmd_rx, gb_tx, shared, Taps { input: inputs, output: outputs }, rec_tx);
        let mut c = self.core.lock();
        // everything is rebuilt for the new rate: hosted plugins keep their state through the project
        self.snapshot_plugin_states(&mut c);
        for id in c.live_fx.drain().collect::<Vec<_>>() {
            self.factory.release(id);
        }
        c.sent_params.clear();
        c.wires = Some(Wires { cmds: cmd_tx, garbage: gb_rx, record: rec_rx, analyzer: Analyzer::new(ain, aout, sample_rate), sr: sample_rate, max_block });
        self.sync(&mut c, Dirty { timeline: true, mixer: true, params: true });
        let flags = Cmd::Flags { metro: c.transport.metro, monitor: c.transport.monitor };
        let watch = watch_cmd(&c.watch);
        send(&mut c, flags);
        send(&mut c, watch);
        c.transport.playing = false;
        rt
    }

    pub fn shared(&self) -> Arc<Shared> {
        self.shared.lock().clone()
    }

    // ------------------------------------------------------------ state & listeners

    pub fn on_change(&self, f: impl Fn(&StateDto) + Send + Sync + 'static) {
        self.listeners.lock().push(Box::new(f));
    }

    pub fn set_frame_sink(&self, f: Option<FrameSink>) {
        *self.sink.lock() = f;
    }

    fn dto(&self, c: &Core) -> StateDto {
        StateDto {
            project: c.project.clone(),
            revision: c.revision,
            can_undo: !c.undo.is_empty(),
            can_redo: !c.redo.is_empty(),
            transport: c.transport.clone(),
            path: c.path.as_ref().map(|p| p.display().to_string()),
            dirty: c.revision != c.saved_revision,
            fx_errors: c.fx_errors.clone(),
        }
    }

    pub fn state(&self) -> StateDto {
        let c = self.core.lock();
        self.dto(&c)
    }

    pub fn project(&self) -> Project {
        self.core.lock().project.clone()
    }

    pub fn revision(&self) -> u64 {
        self.core.lock().revision
    }

    fn notify(&self, dto: StateDto) {
        for l in self.listeners.lock().iter() {
            l(&dto);
        }
    }

    fn changed(&self, c: parking_lot::MutexGuard<'_, Core>) {
        let dto = self.dto(&c);
        drop(c);
        self.notify(dto);
    }

    // ------------------------------------------------------------ edits

    /// Applies a batch. `transient` edits (inside a gesture) do not create undo steps.
    /// With `if_revision`, fails if the project changed since that revision.
    pub fn apply(&self, ops: &[Op], transient: bool, if_revision: Option<u64>) -> Result<u64, EngineError> {
        let mut c = self.core.lock();
        if let Some(r) = if_revision {
            if r != c.revision {
                return Err(EngineError::Msg(format!("REVISION_CONFLICT: project is at revision {}, not {r}", c.revision)));
            }
        }
        let before = c.project.clone();
        let d = ops::apply_all(&mut c.project, ops)?;
        if c.project == before {
            return Ok(c.revision);
        }
        if !(transient && c.gesture.is_some()) {
            push_undo(&mut c, before);
        }
        c.revision += 1;
        self.sync(&mut c, d);
        let rev = c.revision;
        self.changed(c);
        Ok(rev)
    }

    pub fn begin_gesture(&self) {
        let mut c = self.core.lock();
        if c.gesture.is_none() {
            c.gesture = Some(c.project.clone());
        }
    }

    pub fn end_gesture(&self) {
        let mut c = self.core.lock();
        if let Some(g) = c.gesture.take() {
            if g != c.project {
                push_undo(&mut c, g);
                self.changed(c);
            }
        }
    }

    pub fn undo(&self) -> bool {
        let mut c = self.core.lock();
        c.gesture = None;
        let Some(p) = c.undo.pop() else { return false };
        let cur = std::mem::replace(&mut c.project, p);
        c.redo.push(cur);
        c.revision += 1;
        self.sync(&mut c, Dirty { timeline: true, mixer: true, params: true });
        self.changed(c);
        true
    }

    pub fn redo(&self) -> bool {
        let mut c = self.core.lock();
        c.gesture = None;
        let Some(p) = c.redo.pop() else { return false };
        let cur = std::mem::replace(&mut c.project, p);
        c.undo.push(cur);
        c.revision += 1;
        self.sync(&mut c, Dirty { timeline: true, mixer: true, params: true });
        self.changed(c);
        true
    }

    // ------------------------------------------------------------ transport

    pub fn play(&self, from: f64) {
        let mut c = self.core.lock();
        if c.take.is_some() {
            return;
        }
        let from = from.max(0.0);
        let p = &c.project;
        let rec_track = if c.transport.rec { p.tracks.iter().position(|t| t.arm) } else { None };
        let (ls, le) = p.loop_range.map(|l| (l.a, l.b)).unwrap_or((0.0, p.song_end()));
        let from = if rec_track.is_none() && (from >= le || from < ls && p.loop_range.is_some()) { ls } else { from };
        let count_in = if rec_track.is_some() && c.transport.count_in { 4.0 } else { 0.0 };
        if let (Some(track), Some(w)) = (rec_track, c.wires.as_ref()) {
            let sr = w.sr as u32;
            c.take = Some(Take { track, start: from, sr, data: Vec::with_capacity(sr as usize * 2 * 60), peaks: vec![], peaks_sent: 0, acc: (0.0, 0.0, 0) });
        }
        c.transport.start_beat = from;
        c.transport.playing = true;
        send(&mut c, Cmd::Play { from, count_in, rec: rec_track.is_some() });
        self.changed(c);
    }

    pub fn stop(&self) {
        let mut c = self.core.lock();
        send(&mut c, Cmd::Stop);
        c.transport.playing = false;
        drop(c);
        // give the callback a moment to flush the last input
        thread::sleep(Duration::from_millis(30));
        self.finish_take();
        let c = self.core.lock();
        self.changed(c);
    }

    pub fn set_start(&self, beat: f64) {
        let mut c = self.core.lock();
        c.transport.start_beat = beat.max(0.0);
        self.changed(c);
    }

    pub fn set_transport(&self, metro: Option<bool>, count_in: Option<bool>, rec: Option<bool>, monitor: Option<bool>) -> TransportDto {
        let mut c = self.core.lock();
        if let Some(v) = metro {
            c.transport.metro = v;
        }
        if let Some(v) = count_in {
            c.transport.count_in = v;
        }
        if let Some(v) = rec {
            c.transport.rec = v;
        }
        if let Some(v) = monitor {
            c.transport.monitor = v;
        }
        let f = Cmd::Flags { metro: c.transport.metro, monitor: c.transport.monitor };
        send(&mut c, f);
        let t = c.transport.clone();
        self.changed(c);
        t
    }

    pub fn watch_fx(&self, ids: &[Id]) {
        let mut c = self.core.lock();
        c.watch = ids.iter().copied().take(WATCH_SLOTS).collect();
        let w = watch_cmd(&c.watch);
        send(&mut c, w);
    }

    // ------------------------------------------------------------ audio sources

    /// Decodes files (slow; call off the UI thread) and adds them as sources.
    /// With `place`, also puts one clip per file on consecutive tracks starting there.
    pub fn import_audio(&self, paths: &[PathBuf], place: Option<(u32, f64)>) -> Result<(Vec<Id>, Vec<Id>), EngineError> {
        let mut loaded = vec![];
        let mut errors = vec![];
        for p in paths {
            match Audio::load(p) {
                Ok(a) => loaded.push((p.clone(), a)),
                Err(e) => errors.push(e),
            }
        }
        if loaded.is_empty() {
            return Err(EngineError::Msg(errors.join("; ")));
        }
        let mut c = self.core.lock();
        let before = c.project.clone();
        let mut src_ids = vec![];
        let mut clip_ids = vec![];
        let bpm = c.project.bpm;
        for (k, (path, audio)) in loaded.into_iter().enumerate() {
            let id = c.project.alloc_id();
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Audio".into());
            let colors = ["#69c3d6", "#47b8a8", "#5fbf8a", "#7c8cf8", "#e0a44a", "#9a8cf0", "#d9678f", "#5aa9e6"];
            let color = colors[c.project.sources.len() % colors.len()].to_string();
            let src = Source {
                id,
                name,
                color,
                path: path.display().to_string(),
                dur: audio.seconds(),
                sr: audio.sr,
                ch: audio.channels,
                peak_db: crate::dsp::gain_to_db(audio.peak()),
            };
            let len_beats = audio.seconds() / (60.0 / bpm);
            c.sources.insert(id, Arc::new(audio));
            c.project.sources.push(src);
            src_ids.push(id);
            if let Some((track, beat)) = place {
                let ntracks = c.project.tracks.len() as u32;
                if ntracks == 0 {
                    continue;
                }
                let track = (track + k as u32).min(ntracks - 1);
                let cid = c.project.alloc_id();
                let start = beat.max(0.0);
                c.project.clips.push(Clip {
                    id: cid,
                    track,
                    start,
                    len: len_beats.min(TOTAL_BEATS - start).max(1.0 / 16.0),
                    offset: 0.0,
                    kind: ClipKind::Audio,
                    source: id,
                    muted: false,
                    gain: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                    pan: 0.0,
                    norm: false,
                    rev: false,
                });
                clip_ids.push(cid);
            }
        }
        push_undo(&mut c, before);
        c.revision += 1;
        self.sync(&mut c, Dirty { timeline: true, ..Default::default() });
        self.changed(c);
        Ok((src_ids, clip_ids))
    }

    /// [min,max] pairs at 100/s for drawing.
    pub fn source_peaks(&self, id: Id, norm: bool, rev: bool) -> Option<Vec<f32>> {
        let mut c = self.core.lock();
        c.sources.variant(id, norm, rev).map(|a| a.peaks.clone())
    }

    /// Replaces the clip's source by a new file holding only the clip's region.
    pub fn trim_clip(&self, clip_id: Id) -> Result<Id, EngineError> {
        let mut c = self.core.lock();
        let clip = c.project.clips.iter().find(|x| x.id == clip_id).cloned().ok_or(EngineError::Msg("no such clip".into()))?;
        if clip.kind != ClipKind::Audio {
            return Err(EngineError::Msg("only audio clips can be trimmed".into()));
        }
        let audio = c.sources.variant(clip.source, clip.norm, clip.rev).ok_or(EngineError::Msg("audio not loaded".into()))?;
        let spb = 60.0 / c.project.bpm;
        let sr = audio.sr as f64;
        let a = ((clip.offset * spb * sr) as usize).min(audio.frames);
        let b = (((clip.offset + clip.len) * spb * sr) as usize).min(audio.frames);
        if b <= a + 16 {
            return Err(EngineError::Msg("the region is empty".into()));
        }
        let data = audio.content()[a * 2..b * 2].to_vec();
        let old = c.project.sources.iter().find(|s| s.id == clip.source).cloned().unwrap();
        let id = c.project.alloc_id();
        let path = audio_dir(&c).join(format!("{} trim {}.wav", sanitize(&old.name), id));
        write_wav(&path, &data, audio.sr)?;
        let new = Audio::from_stereo(data, audio.sr, audio.channels);
        let before = c.project.clone();
        c.project.sources.push(Source { id, path: path.display().to_string(), dur: new.seconds(), peak_db: crate::dsp::gain_to_db(new.peak()), ..old });
        c.sources.insert(id, Arc::new(new));
        if let Some(x) = c.project.clips.iter_mut().find(|x| x.id == clip_id) {
            x.source = id;
            x.offset = 0.0;
            x.norm = false;
            x.rev = false;
        }
        push_undo(&mut c, before);
        c.revision += 1;
        self.sync(&mut c, Dirty { timeline: true, ..Default::default() });
        self.changed(c);
        Ok(id)
    }

    pub fn preview_clip(&self, clip_id: Id) -> Result<(), EngineError> {
        let mut c = self.core.lock();
        let clip = c.project.clips.iter().find(|x| x.id == clip_id).cloned().ok_or(EngineError::Msg("no such clip".into()))?;
        let audio = c.sources.variant(clip.source, clip.norm, clip.rev).ok_or(EngineError::Msg("audio not loaded".into()))?;
        let spb = 60.0 / c.project.bpm;
        let sr = audio.sr as f64;
        let pv = Preview {
            pos: clip.offset * spb * sr,
            start: clip.offset * spb * sr,
            end: ((clip.offset + clip.len) * spb * sr).min(audio.frames as f64),
            gain: 10f32.powf(clip.gain / 20.0),
            fade_in: clip.fade_in as f64,
            fade_out: clip.fade_out as f64,
            audio,
        };
        send(&mut c, Cmd::Preview(Some(Box::new(pv))));
        Ok(())
    }

    pub fn preview_file(&self, path: &Path) -> Result<(), EngineError> {
        let audio = Arc::new(Audio::load(path)?);
        let mut c = self.core.lock();
        let end = audio.frames as f64;
        send(&mut c, Cmd::Preview(Some(Box::new(Preview { audio, pos: 0.0, start: 0.0, end, gain: 0.8, fade_in: 0.0, fade_out: 0.0 }))));
        Ok(())
    }

    pub fn preview_stop(&self) {
        let mut c = self.core.lock();
        send(&mut c, Cmd::Preview(None));
    }

    // ------------------------------------------------------------ project files

    pub fn new_project(&self, project: Project) {
        let mut c = self.core.lock();
        if c.take.is_some() {
            return;
        }
        send(&mut c, Cmd::Stop);
        c.transport.playing = false;
        c.project = project;
        c.undo.clear();
        c.redo.clear();
        c.path = None;
        c.revision += 1;
        c.saved_revision = c.revision;
        c.fx_errors.clear();
        self.sync(&mut c, Dirty { timeline: true, mixer: true, params: true });
        self.changed(c);
    }

    pub fn save(&self, path: &Path) -> Result<(), EngineError> {
        let mut c = self.core.lock();
        self.snapshot_plugin_states(&mut c);
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut p = c.project.clone();
        // move recordings that live in the scratch folder next to the project
        let audio_dir = dir.join(format!("{} Audio", path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()));
        let scratch = scratch_dir();
        for s in p.sources.iter_mut() {
            let sp = PathBuf::from(&s.path);
            if sp.starts_with(&scratch) {
                let dst = audio_dir.join(sp.file_name().unwrap_or_default());
                std::fs::create_dir_all(&audio_dir).map_err(|e| e.to_string())?;
                std::fs::copy(&sp, &dst).map_err(|e| format!("copying {}: {e}", sp.display()))?;
                s.path = dst.display().to_string();
            }
            if let Ok(rel) = Path::new(&s.path).strip_prefix(dir) {
                s.path = rel.display().to_string();
            }
        }
        let doc = serde_json::json!({ "format": "wave-project", "version": 2, "project": p });
        let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("wave.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        // keep absolute paths in memory
        for s in c.project.sources.iter_mut() {
            let sp = PathBuf::from(&s.path);
            if sp.starts_with(&scratch) {
                s.path = audio_dir.join(sp.file_name().unwrap_or_default()).display().to_string();
            }
        }
        c.path = Some(path.to_path_buf());
        c.saved_revision = c.revision;
        self.changed(c);
        Ok(())
    }

    /// Loads a project file. Missing audio is reported, not fatal.
    pub fn open(&self, path: &Path) -> Result<Vec<String>, EngineError> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("not a WAVE project: {e}"))?;
        let mut p: Project = serde_json::from_value(doc.get("project").cloned().unwrap_or(doc)).map_err(|e| format!("not a WAVE project: {e}"))?;
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut warnings = vec![];
        let mut store = SourceStore::default();
        for s in p.sources.iter_mut() {
            let sp = PathBuf::from(&s.path);
            let abs = if sp.is_absolute() { sp } else { dir.join(sp) };
            s.path = abs.display().to_string();
            match Audio::load(&abs) {
                Ok(a) => store.insert(s.id, Arc::new(a)),
                Err(e) => warnings.push(e),
            }
        }
        if p.inserts.first().map(|m| m.id) != Some(0) {
            return Err(EngineError::Msg("the project has no master insert".into()));
        }
        let mut c = self.core.lock();
        send(&mut c, Cmd::Stop);
        c.transport.playing = false;
        c.sources = store;
        c.project = p;
        c.undo.clear();
        c.redo.clear();
        c.path = Some(path.to_path_buf());
        c.revision += 1;
        c.saved_revision = c.revision;
        c.fx_errors.clear();
        self.sync(&mut c, Dirty { timeline: true, mixer: true, params: true });
        self.changed(c);
        Ok(warnings)
    }

    // ------------------------------------------------------------ offline render

    /// Renders `[from, to)` beats (plus `tail` seconds) of the current project to
    /// interleaved stereo at `sample_rate`, independently of the live device.
    pub fn render(&self, from: f64, to: f64, tail: f64, sample_rate: u32) -> Result<Vec<f32>, EngineError> {
        let (project, store_snapshot) = {
            let mut guard = self.core.lock();
            let c = &mut *guard;
            self.snapshot_plugin_states(c);
            let mut map = HashMap::new();
            for clip in &c.project.clips {
                if clip.kind == ClipKind::Audio {
                    if let Some(a) = c.sources.variant(clip.source, clip.norm, clip.rev) {
                        map.insert((clip.source, clip.norm, clip.rev), a);
                    }
                }
            }
            (c.project.clone(), map)
        };
        let sr = sample_rate as f32;
        let block = 512;
        let (mut cmd_tx, cmd_rx) = rtrb::RingBuffer::<Cmd>::new(64);
        let (gb_tx, mut gb_rx) = rtrb::RingBuffer::<Garbage>::new(64);
        let (rec_tx, _rec_rx) = rtrb::RingBuffer::<f32>::new(16);
        let mut taps = Taps { input: vec![], output: vec![] };
        let mut keep = vec![];
        for _ in 0..WATCH_SLOTS {
            let (a, b) = rtrb::RingBuffer::<f32>::new(16);
            let (c2, d) = rtrb::RingBuffer::<f32>::new(16);
            taps.input.push(a);
            taps.output.push(c2);
            keep.push((b, d));
        }
        let mut rt = Rt::new(sr, block, cmd_rx, gb_tx, Arc::new(Shared::new()), taps, rec_tx);
        let mut errs = HashMap::new();
        let graph = build_graph(&project, &HashSet::new(), sr, block, &*self.factory, false, &mut errs);
        let mut tl = build_timeline(&project, &mut |id, n, r| store_snapshot.get(&(id, n, r)).cloned());
        tl.loop_range = None;
        tl.song_end = TOTAL_BEATS;
        let _ = cmd_tx.push(Cmd::Graph(Box::new(graph)));
        let _ = cmd_tx.push(Cmd::Timeline(Box::new(tl)));
        let _ = cmd_tx.push(Cmd::Play { from, count_in: 0.0, rec: false });
        let spb = 60.0 / project.bpm;
        let frames = (((to - from).max(0.0) * spb + tail.max(0.0)) * sample_rate as f64) as usize;
        let play_frames = ((to - from).max(0.0) * spb * sample_rate as f64) as usize;
        let mut out = Vec::with_capacity(frames * 2);
        let mut l = vec![0f32; block];
        let mut r = vec![0f32; block];
        let mut done = 0;
        let mut stopped = false;
        while done < frames {
            let n = block.min(frames - done);
            if !stopped && done + n > play_frames {
                // stop exactly at `to`: render up to it, then let tails ring
                let k = play_frames.saturating_sub(done);
                if k > 0 {
                    rt.process(&mut l[..k], &mut r[..k], None);
                    for i in 0..k {
                        out.push(l[i]);
                        out.push(r[i]);
                    }
                    done += k;
                }
                let _ = cmd_tx.push(Cmd::Stop);
                stopped = true;
                continue;
            }
            rt.process(&mut l[..n], &mut r[..n], None);
            for i in 0..n {
                out.push(l[i]);
                out.push(r[i]);
            }
            done += n;
            while gb_rx.pop().is_ok() {}
        }
        Ok(out)
    }

    // ------------------------------------------------------------ internals

    fn snapshot_plugin_states(&self, c: &mut Core) {
        let factory = &self.factory;
        for m in c.project.inserts.iter_mut() {
            for f in m.fx.iter_mut() {
                if let Some(pl) = f.plugin.as_mut() {
                    if let Some(st) = factory.state(f.id) {
                        pl.state = Some(st);
                    }
                }
            }
        }
    }

    /// Pushes the project to the audio thread.
    fn sync(&self, c: &mut Core, d: Dirty) {
        let Some(w) = c.wires.as_ref() else { return };
        let (sr, max_block) = (w.sr, w.max_block);
        if d.mixer {
            let now: HashSet<Id> = c.project.inserts.iter().flat_map(|m| m.fx.iter().map(|f| f.id)).collect();
            for gone in c.live_fx.difference(&now).copied().collect::<Vec<_>>() {
                self.factory.release(gone);
                c.fx_errors.remove(&gone);
            }
            let mut errs = std::mem::take(&mut c.fx_errors);
            let graph = build_graph(&c.project, &c.live_fx, sr, max_block, &*self.factory, true, &mut errs);
            c.fx_errors = errs;
            c.live_fx = graph.inserts.iter().flat_map(|i| i.fx.iter().map(|s| s.id)).collect();
            c.sent_params = c.project.inserts.iter().flat_map(|m| m.fx.iter()).filter(|f| f.kind != FxKind::Vst3).map(|f| (f.id, param_block(f))).collect();
            send(c, Cmd::Graph(Box::new(graph)));
        } else if d.params {
            send(c, Cmd::Mix(Box::new(mix_state(&c.project))));
            let changed: Vec<(Id, ParamBlock)> = c
                .project
                .inserts
                .iter()
                .flat_map(|m| m.fx.iter())
                .filter(|f| f.kind != FxKind::Vst3)
                .map(|f| (f.id, param_block(f)))
                .filter(|(id, pb)| c.sent_params.get(id) != Some(pb))
                .collect();
            for (id, pb) in changed {
                c.sent_params.insert(id, pb);
                send(c, Cmd::Params { fx: id, params: pb });
            }
        }
        if d.timeline {
            let Core { project, sources, .. } = c;
            let tl = build_timeline(project, &mut |id, n, r| sources.variant(id, n, r));
            send(c, Cmd::Timeline(Box::new(tl)));
        }
    }

    fn housekeeping(&self, tick: u32) {
        let shared = self.shared();
        let frame_parts;
        {
            let mut guard = self.core.lock();
            let c = &mut *guard;
            let Some(w) = c.wires.as_mut() else { return };
            while w.garbage.pop().is_ok() {}
            let analysis = if tick % 2 == 0 { Some(w.analyzer.analyze(&c_watch(&c.watch), &shared)) } else { None };
            let w = c.wires.as_mut().unwrap();
            let sr = w.sr as u32;
            // drain recorded input
            let n = w.record.slots();
            if n > 0 {
                if let Ok(chunk) = w.record.read_chunk(n) {
                    let (a, b) = chunk.as_slices();
                    if let Some(t) = c.take.as_mut() {
                        for &x in a.iter().chain(b.iter()) {
                            t.data.push(x);
                        }
                        let per = (t.sr as usize / 100).max(1);
                        while (t.acc.2 + 1) * 2 <= t.data.len() {
                            let f = t.acc.2;
                            let (l, r) = (t.data[f * 2], t.data[f * 2 + 1]);
                            let v = (l + r) * 0.5;
                            t.acc.0 = t.acc.0.min(v);
                            t.acc.1 = t.acc.1.max(v);
                            t.acc.2 += 1;
                            if t.acc.2 % per == 0 {
                                t.peaks.push(t.acc.0);
                                t.peaks.push(t.acc.1);
                                t.acc.0 = 0.0;
                                t.acc.1 = 0.0;
                            }
                        }
                    }
                    chunk.commit_all();
                }
            }
            let _ = sr;
            let ids: Vec<Id> = c.project.inserts.iter().map(|m| m.id).collect();
            let rec = c.take.as_mut().map(|t| {
                let new = t.peaks[t.peaks_sent..].to_vec();
                let off = t.peaks_sent / 2;
                t.peaks_sent = t.peaks.len();
                (t.track as u32, t.start, t.data.len() as f32 / 2.0 / t.sr as f32, off as u32, new)
            });
            frame_parts = Some((ids, analysis, rec));
        }
        if shared.stopped_at_end.swap(false, Relaxed) {
            {
                let mut c = self.core.lock();
                c.transport.playing = false;
            }
            self.finish_take();
            let c = self.core.lock();
            self.changed(c);
        }
        if let Some((ids, analysis, rec)) = frame_parts {
            let sink = self.sink.lock();
            if let Some(sink) = sink.as_ref() {
                let frame = crate::telemetry::frame(tick, &shared, &ids, analysis.as_deref(), rec.as_ref());
                sink(frame);
            } else {
                // still consume meters so the first frame after subscribing is fresh
                for i in 0..ids.len().min(MAX_INSERTS) {
                    shared.take_meter(i);
                }
            }
        }
    }

    fn finish_take(&self) {
        let mut c = self.core.lock();
        let Some(take) = c.take.take() else { return };
        // drain whatever is left in the ring
        if let Some(w) = c.wires.as_mut() {
            while let Ok(x) = w.record.pop() {
                let _ = x;
            }
        }
        let secs = take.data.len() as f64 / 2.0 / take.sr as f64;
        if secs < 0.05 {
            return;
        }
        let id = c.project.alloc_id();
        let n = c.project.sources.iter().filter(|s| s.name.starts_with("Take")).count() + 1;
        let name = format!("Take {n}");
        let path = audio_dir(&c).join(format!("{} {}.wav", name, id));
        if let Err(e) = write_wav(&path, &take.data, take.sr) {
            log::error!("could not write recording: {e}");
        }
        let audio = Audio::from_stereo(take.data, take.sr, 1);
        let before = c.project.clone();
        let len = secs / (60.0 / c.project.bpm);
        c.project.sources.push(Source { id, name, color: "#e05263".into(), path: path.display().to_string(), dur: secs, sr: take.sr, ch: 1, peak_db: crate::dsp::gain_to_db(audio.peak()) });
        c.sources.insert(id, Arc::new(audio));
        let cid = c.project.alloc_id();
        c.project.clips.push(Clip {
            id: cid,
            track: take.track as u32,
            start: take.start,
            len: len.min(TOTAL_BEATS - take.start),
            offset: 0.0,
            kind: ClipKind::Audio,
            source: id,
            muted: false,
            gain: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            pan: 0.0,
            norm: false,
            rev: false,
        });
        push_undo(&mut c, before);
        c.revision += 1;
        self.sync(&mut c, Dirty { timeline: true, ..Default::default() });
    }
}

fn c_watch(w: &[Id]) -> Vec<Id> {
    w.to_vec()
}

fn push_undo(c: &mut Core, before: Project) {
    c.undo.push(before);
    if c.undo.len() > UNDO_LIMIT {
        c.undo.remove(0);
    }
    c.redo.clear();
}

fn send(c: &mut Core, cmd: Cmd) {
    if let Some(w) = c.wires.as_mut() {
        if w.cmds.push(cmd).is_err() {
            log::warn!("audio command queue full; dropping a command");
        }
    }
}

fn watch_cmd(ids: &[Id]) -> Cmd {
    let mut a = [u64::MAX; WATCH_SLOTS];
    for (i, id) in ids.iter().take(WATCH_SLOTS).enumerate() {
        a[i] = *id;
    }
    Cmd::Watch { ids: a, n: ids.len().min(WATCH_SLOTS) as u8 }
}

pub fn scratch_dir() -> PathBuf {
    dirs_data().join("WAVE").join("Recordings")
}

fn dirs_data() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(p);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join("Library/Application Support");
        }
    }
    if let Some(p) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(p);
    }
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h).join(".local/share");
    }
    std::env::temp_dir()
}

fn audio_dir(c: &Core) -> PathBuf {
    match &c.path {
        Some(p) => p.parent().unwrap_or(Path::new(".")).join(format!("{} Audio", p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())),
        None => scratch_dir(),
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|ch| if ch.is_alphanumeric() || ch == ' ' || ch == '-' || ch == '_' { ch } else { '_' }).collect()
}

fn audible_tracks(p: &Project) -> Vec<(f32, bool)> {
    let any_solo = p.tracks.iter().any(|t| t.solo);
    p.tracks.iter().map(|t| (fader_gain(t.vol), !t.muted && (!any_solo || t.solo))).collect()
}

fn audible_inserts(p: &Project) -> Vec<(Id, f32, f32, bool)> {
    let any_solo = p.inserts.iter().any(|m| m.id != 0 && m.solo);
    p.inserts.iter().map(|m| (m.id, fader_gain(m.vol), m.pan.clamp(-1.0, 1.0), !m.muted && (m.id == 0 || !any_solo || m.solo))).collect()
}

fn monitor_track(p: &Project) -> Option<usize> {
    p.tracks.iter().position(|t| t.arm)
}

fn mix_state(p: &Project) -> MixState {
    MixState { inserts: audible_inserts(p), tracks: audible_tracks(p), monitor_track: monitor_track(p) }
}

/// Builds the mixer graph. Effects in `live` are left as `proc: None` so the audio
/// thread keeps the running instance; everything else is created and prepared here.
pub(crate) fn build_graph(p: &Project, live: &HashSet<Id>, sr: f32, max_block: usize, factory: &dyn PluginFactory, live_instances: bool, errs: &mut HashMap<Id, String>) -> Graph {
    let ins_state = audible_inserts(p);
    let inserts = p
        .inserts
        .iter()
        .zip(ins_state)
        .map(|(m, (id, gain, pan, on))| {
            let fx = m
                .fx
                .iter()
                .map(|f| {
                    let proc: Option<Box<dyn Processor>> = if live.contains(&f.id) {
                        None
                    } else if f.kind == FxKind::Vst3 {
                        match factory.create(f.id, f.plugin.as_ref().unwrap(), sr, max_block, live_instances) {
                            Ok(p) => {
                                errs.remove(&f.id);
                                Some(p)
                            }
                            Err(e) => {
                                errs.insert(f.id, e);
                                None
                            }
                        }
                    } else {
                        let mut pr = crate::dsp::create(f.kind).unwrap();
                        pr.prepare(sr, max_block);
                        pr.set_params(&param_block(f));
                        Some(pr)
                    };
                    let params = if f.kind == FxKind::Vst3 { None } else { Some(param_block(f)) };
                    FxSlot { id: f.id, bypass: f.bypass, proc, params }
                })
                .collect();
            InsertRt::new(id, gain, pan, on, fx, max_block)
        })
        .collect();
    let track_state = audible_tracks(p);
    let tracks = p
        .tracks
        .iter()
        .zip(track_state)
        .map(|(t, (gain, on))| TrackRt { insert: p.inserts.iter().position(|m| m.id == t.insert).unwrap_or(0), gain, on })
        .collect();
    Graph::new(inserts, tracks, monitor_track(p), max_block)
}

pub(crate) fn build_timeline(p: &Project, audio: &mut dyn FnMut(Id, bool, bool) -> Option<Arc<Audio>>) -> Timeline {
    let pats: Vec<PatRt> = p
        .patterns
        .iter()
        .map(|pt| PatRt {
            len: pt.len,
            notes: pt.notes.iter().map(|n| NoteRt { beat: n.step as f64 / 4.0, len: n.len as f64 / 4.0, inst: n.inst, key: n.p }).collect(),
        })
        .collect();
    let mut clips = vec![];
    for c in p.clips.iter().filter(|c| !c.muted) {
        if c.track as usize >= p.tracks.len() {
            continue;
        }
        let src = match c.kind {
            ClipKind::Pat => match p.patterns.iter().position(|x| x.id == c.source) {
                Some(i) => ClipSrc::Pat(i),
                None => continue,
            },
            ClipKind::Audio => match audio(c.source, c.norm, c.rev) {
                Some(a) => ClipSrc::Audio(a),
                None => continue,
            },
        };
        clips.push(ClipRt { track: c.track as usize, start: c.start, end: c.start + c.len, offset: c.offset, gain: c.gain, pan: c.pan, fade_in: c.fade_in, fade_out: c.fade_out, src });
    }
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    Timeline { bpm: p.bpm, clips, pats, loop_range: p.loop_range.map(|l| (l.a, l.b)), song_end: p.song_end() }
}
