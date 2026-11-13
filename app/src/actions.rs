//! Everything the GUI and the CLI can ask for, implemented once.
//!
//! The actions run against a [`Host`]: the running app (`App`), or a project file opened
//! by `wave --project FILE` with no window and no audio device (`headless::Headless`).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use wave_engine::catalog::{self, load_preset, new_fx};
use wave_engine::{Engine, FxKind, Op, PluginRef, RefTune};

use crate::plugins::Plugins;

pub type R<T> = Result<T, String>;

/// What an action needs from the process it runs in.
pub trait Host {
    fn engine(&self) -> &Arc<Engine>;
    fn plugins(&self) -> &Arc<Plugins>;
    /// True in the running app: an audio device, a live undo history and a musician.
    /// False for a project file edited from the command line.
    fn live(&self) -> bool;
    fn audio_json(&self) -> Value;
    fn cli_json(&self) -> Value;
    /// Why the installed RMVPE model could not be used, if it could not.
    fn model_error(&self) -> Option<String>;
    /// Before an offline bounce: the WAVE Keys piano the project plays is loaded.
    fn wait_keys(&self) -> R<()>;
    /// Before pitch work or a bounce of pitch-edited clips: the pitch model is in place.
    fn ensure_pitch_model(&self) {}
    /// Before VST3 parameter access: the project's plugins run as live instances.
    fn ensure_live_plugins(&self) {}
}

/// Commands that need the audio device, the musician's undo history or the app's window.
pub const LIVE_ONLY: &[&str] = &["play", "stop", "pattern_play", "comp_audition", "note_audition", "undo", "redo", "open"];

fn live_only_error(cmd: &str) -> String {
    let why = match cmd {
        "undo" | "redo" => "a project file has no undo history (each `wave --project` call starts fresh; edit with `apply` and keep the file under version control, or use --dry-run / --save-as to try an edit)",
        "open" => "pass the file with --project instead",
        _ => "it plays through the audio device; render with `wave --project FILE render --out x.wav` to listen",
    };
    format!("NEEDS_APP: `{cmd}` needs the running app: {why}")
}

fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a Value> {
    a.get(k).filter(|v| !v.is_null())
}
fn as_index(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0 && f.fract() == 0.0).map(|f| f as u64))
}
fn need_u64(a: &Value, k: &str) -> R<u64> {
    arg(a, k).and_then(as_index).ok_or(format!("`{k}` (whole number) is required"))
}
fn need_str<'a>(a: &'a Value, k: &str) -> R<&'a str> {
    arg(a, k).and_then(Value::as_str).ok_or(format!("`{k}` (string) is required"))
}
fn opt_f64(a: &Value, k: &str) -> Option<f64> {
    arg(a, k).and_then(Value::as_f64)
}

pub fn parse_kind(s: &str) -> R<FxKind> {
    serde_json::from_value(json!(s)).map_err(|_| {
        format!("unknown effect type `{s}` (eq, comp, sat, delay, half, warp, reverb, limit, fet, deess, tune, denoise, voice, rider)")
    })
}

pub fn add_fx(app: &dyn Host, insert_id: u64, kind: FxKind, preset: Option<usize>, index: Option<usize>) -> R<(u64, u64)> {
    if kind == FxKind::Vst3 {
        return Err("use vst3_add for plugins".into());
    }
    let p = app.engine().project();
    if p.insert(insert_id).is_none() {
        return Err(format!("no insert {insert_id}"));
    }
    let id = p.next_id;
    let mut fx = new_fx(id, kind, p.bpm);
    if let Some(i) = preset {
        if !load_preset(&mut fx, i, p.bpm) {
            return Err(format!("{} has no preset {i}", catalog::type_name(kind)));
        }
    }
    let rev = app.engine().apply(&[Op::PutFx { insert_id, fx, index }], false, None).map_err(|e| e.to_string())?;
    Ok((id, rev))
}

pub fn set_param(app: &dyn Host, fx_id: u64, param: &str, value: f64, if_revision: Option<u64>) -> R<u64> {
    let p = app.engine().project();
    let (m, f) = p.find_fx(fx_id).ok_or(format!("no effect {fx_id}"))?;
    let def = catalog::params(f.kind).iter().find(|d| d.id == param).ok_or_else(|| {
        format!(
            "{} has no parameter `{param}` (has: {})",
            catalog::type_name(f.kind),
            catalog::params(f.kind).iter().map(|d| d.id).collect::<Vec<_>>().join(", ")
        )
    })?;
    if !(def.min..=def.max).contains(&value) {
        return Err(format!("{param} must be within {}..{} {}", def.min, def.max, def.unit));
    }
    let mut f = f.clone();
    f.params.insert(param.into(), value);
    app.engine().apply(&[Op::PutFx { insert_id: m.id, fx: f, index: None }], false, if_revision).map_err(|e| e.to_string())
}

pub fn preset(app: &dyn Host, fx_id: u64, index: usize) -> R<u64> {
    let p = app.engine().project();
    let (m, f) = p.find_fx(fx_id).ok_or(format!("no effect {fx_id}"))?;
    let mut f = f.clone();
    if !load_preset(&mut f, index, p.bpm) {
        return Err(format!("no preset {index}"));
    }
    app.engine().apply(&[Op::PutFx { insert_id: m.id, fx: f, index: None }], false, None).map_err(|e| e.to_string())
}

pub fn vst3_add(app: &dyn Host, insert_id: u64, path: &str, cid: &str, index: Option<usize>) -> R<(u64, u64)> {
    let p = app.engine().project();
    if p.insert(insert_id).is_none() {
        return Err(format!("no insert {insert_id}"));
    }
    let (name, vendor) = app.plugins().info(path, cid).ok_or("that plugin is not in the scan results; run a VST3 scan first")?;
    // the load error below comes from instantiating the plugin live
    app.ensure_live_plugins();
    let id = p.next_id;
    let fx = wave_engine::Fx {
        id,
        kind: FxKind::Vst3,
        bypass: false,
        mix: 1.0,
        preset: None,
        params: Default::default(),
        bands: None,
        plugin: Some(PluginRef { format: "vst3".into(), path: path.into(), cid: cid.into(), name, vendor, state: None }),
        maps: None,
    };
    let rev = app.engine().apply(&[Op::PutFx { insert_id, fx, index }], false, None).map_err(|e| e.to_string())?;
    if let Some(err) = app.engine().state().fx_errors.get(&id) {
        return Err(format!("added, but the plugin failed to load: {err}"));
    }
    Ok((id, rev))
}

/// Level statistics of interleaved stereo.
pub fn stats(x: &[f32], sr: u32) -> Value {
    let n = x.len().max(1);
    let peak = x.iter().fold(0f32, |a, b| a.max(b.abs()));
    let rms = (x.iter().map(|v| (v * v) as f64).sum::<f64>() / n as f64).sqrt() as f32;
    let clipped = x.iter().filter(|v| v.abs() >= 0.999).count();
    let db = |g: f32| if g > 1e-9 { (20.0 * (g as f64).log10() * 10.0).round() / 10.0 } else { -180.0 };
    json!({
        "seconds": x.len() as f64 / 2.0 / sr as f64,
        "sampleRate": sr,
        "peakDbfs": db(peak),
        "rmsDbfs": db(rms),
        "crestDb": ((db(peak) - db(rms)) * 10.0).round() / 10.0,
        "clippedSamples": clipped,
        "silent": peak < 1e-5,
    })
}

/// Output container of an offline bounce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Wav(WavEncoding),
    Mp3 { kbps: u32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WavEncoding {
    #[default]
    Float32,
    Pcm24 {
        dither: bool,
    },
}

impl WavEncoding {
    pub fn parse(bits: Option<u32>, dither: bool) -> R<Self> {
        match bits.unwrap_or(32) {
            24 => Ok(Self::Pcm24 { dither }),
            32 if !dither => Ok(Self::Float32),
            32 => Err("dither requires 24-bit PCM WAV".into()),
            _ => Err("WAV bit depth must be 24 (PCM) or 32 (float)".into()),
        }
    }

    fn write(self, out: &std::path::Path, audio: &[f32], rate: u32) -> R<()> {
        use wave_engine::sources::{write_wav, write_wav_pcm24};
        match self {
            Self::Float32 => write_wav(out, audio, rate),
            Self::Pcm24 { dither } => write_wav_pcm24(out, audio, rate, dither),
        }
    }
}

impl ExportFormat {
    /// `format`: "wav" (default) or "mp3"; `kbps`: 128/192/256/320 (default 320, mp3 only).
    pub fn parse(format: Option<&str>, kbps: Option<u32>, wav_bits: Option<u32>, dither: bool) -> R<Self> {
        match format.map(str::to_ascii_lowercase).as_deref() {
            None | Some("wav") => Ok(Self::Wav(WavEncoding::parse(wav_bits, dither)?)),
            Some("mp3") => {
                if wav_bits.is_some() || dither {
                    return Err("WAV bit depth and dither apply only to WAV export".into());
                }
                let kbps = kbps.unwrap_or(320);
                if ![128, 192, 256, 320].contains(&kbps) {
                    return Err("mp3Kbps must be 128, 192, 256 or 320".into());
                }
                Ok(Self::Mp3 { kbps })
            }
            Some(o) => Err(format!("format must be wav or mp3, not `{o}`")),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav(_) => "wav",
            Self::Mp3 { .. } => "mp3",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Bounce {
    pub from: f64,
    pub to: f64,
    pub tail: f64,
    pub rate: u32,
    pub wrap: bool,
}

impl Bounce {
    fn render(self, engine: &wave_engine::Engine, insert: Option<u64>, cancel: &std::sync::atomic::AtomicBool) -> R<Vec<f32>> {
        use wave_engine::RenderTail;
        let tail = if self.wrap { RenderTail::Wrap } else { RenderTail::Seconds(self.tail) };
        engine.render_export(insert, self.from, self.to, tail, self.rate, cancel).map_err(|e| e.to_string())
    }

    fn tail_seconds(self) -> f64 {
        if self.wrap {
            0.0
        } else {
            self.tail
        }
    }
}

/// Offline bounce shared by the GUI export dialog and `wave render`.
pub fn render_to_file(
    app: &dyn Host,
    out: &std::path::Path,
    bounce: Bounce,
    cancel: &std::sync::atomic::AtomicBool,
    fmt: ExportFormat,
) -> R<Value> {
    if bounce.wrap && !matches!(fmt, ExportFormat::Wav(_)) {
        return Err("wrap tail requires WAV export".into());
    }
    // a bounce must not start while the piano is still loading (it would be silent)
    app.wait_keys()?;
    let audio = bounce.render(app.engine(), None, cancel)?;
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    match fmt {
        ExportFormat::Wav(encoding) => encoding.write(out, &audio, bounce.rate)?,
        ExportFormat::Mp3 { kbps } => {
            let title = app
                .engine()
                .state()
                .path
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()));
            wave_engine::sources::write_mp3(out, &audio, bounce.rate, kbps, title.as_deref())?
        }
    }
    Ok(json!({
        "out": out.display().to_string(),
        "fromBeat": bounce.from,
        "toBeat": bounce.to,
        "tail": bounce.tail_seconds(),
        "wrapTail": bounce.wrap,
        "format": fmt.extension(),
        "stats": stats(&audio, bounce.rate),
    }))
}

/// A file name for `name` that is valid on Windows, macOS and Linux: characters the file systems
/// refuse become `_`, trailing dots and spaces go, a reserved device name (`CON`, `NUL`, …) gets a
/// leading `_`, and an empty or very long name is replaced or cut.
pub fn safe_file_stem(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).chars().take(100).collect::<String>();
    let cleaned = cleaned.trim_end().to_string();
    if cleaned.is_empty() {
        return "Insert".into();
    }
    let base = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.len() == 4
            && base.as_bytes()[3].is_ascii_digit()
            && base.as_bytes()[3] != b'0');
    if reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

/// File stems for `names` in one folder: made safe by [`safe_file_stem`] and made unique with a
/// ` (2)`, ` (3)` suffix. Compared without regard to case, because Windows and macOS do.
pub fn unique_file_stems(names: &[String]) -> Vec<String> {
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    names
        .iter()
        .map(|n| {
            let base = safe_file_stem(n);
            let mut cand = base.clone();
            let mut k = 2;
            while !taken.insert(cand.to_lowercase()) {
                cand = format!("{base} ({k})");
                k += 1;
            }
            cand
        })
        .collect()
}

/// Stem export, like FL Studio's *Split mixer tracks*: one stereo WAV per mixer insert
/// that has sound (`Engine::stem_inserts`), named after the insert, written into `dir`; plus
/// `Master.wav`, the normal mix with the master's effects, when `include_master`. Every file has
/// the same length and starts at `from`, so they line up when dropped into another project.
///
/// Nothing is left behind on cancel or error: files are written as `*.wav.part` and renamed only
/// once every stem has rendered. `progress(done, total, name)` is called before each file starts.
pub fn render_stems_to_dir(
    engine: &wave_engine::Engine,
    dir: &std::path::Path,
    bounce: Bounce,
    cancel: &std::sync::atomic::AtomicBool,
    include_master: bool,
    encoding: WavEncoding,
    progress: &dyn Fn(usize, usize, &str),
) -> R<Value> {
    use std::sync::atomic::Ordering::Relaxed;
    if bounce.to <= bounce.from {
        return Err("`to` must be after `from`".into());
    }
    let project = engine.project();
    // (insert id — None is the master mix, display name)
    let mut jobs: Vec<(Option<u64>, String)> = vec![];
    if include_master {
        jobs.push((None, "Master".into()));
    }
    for (id, name) in wave_engine::Engine::stem_inserts(&project) {
        jobs.push((Some(id), if name.trim().is_empty() { format!("Insert {id}") } else { name }));
    }
    if jobs.is_empty() {
        return Err("nothing to export: every mixer track is muted or has no sound routed to it".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let stems = unique_file_stems(&jobs.iter().map(|j| j.1.clone()).collect::<Vec<_>>());
    let total = jobs.len();
    let mut parts: Vec<(PathBuf, PathBuf)> = vec![];
    let mut files: Vec<Value> = vec![];
    let mut skipped: Vec<String> = vec![];
    let rendered: R<()> = (|| {
        for (i, (job, name)) in jobs.iter().enumerate() {
            progress(i, total, name);
            let audio = bounce.render(engine, *job, cancel)?;
            if cancel.load(Relaxed) {
                return Err("cancelled".to_string());
            }
            let st = stats(&audio, bounce.rate);
            // an insert that only looked busy (an empty pattern, an automation clip) gets no file
            if job.is_some() && st["silent"] == json!(true) {
                skipped.push(name.clone());
                continue;
            }
            let file = format!("{}.wav", stems[i]);
            let part = dir.join(format!("{file}.part"));
            if let Err(error) = encoding.write(&part, &audio, bounce.rate) {
                let _ = std::fs::remove_file(&part);
                return Err(format!("cannot write {file}: {error}"));
            }
            parts.push((part, dir.join(&file)));
            files.push(json!({ "name": name, "file": file, "master": job.is_none(), "stats": st }));
        }
        if parts.is_empty() {
            return Err("nothing to export: everything rendered silent".into());
        }
        Ok(())
    })();
    if let Err(e) = rendered {
        for (part, _) in &parts {
            let _ = std::fs::remove_file(part);
        }
        return Err(e);
    }
    for (k, (part, fin)) in parts.iter().enumerate() {
        if let Err(e) = std::fs::rename(part, fin) {
            for (rest, _) in &parts[k..] {
                let _ = std::fs::remove_file(rest);
            }
            return Err(format!("cannot write {}: {e}", fin.display()));
        }
    }
    progress(total, total, "");
    Ok(json!({
        "dir": dir.display().to_string(),
        "fromBeat": bounce.from,
        "toBeat": bounce.to,
        "tail": bounce.tail_seconds(),
        "wrapTail": bounce.wrap,
        "format": "wav",
        "files": files,
        "skipped": skipped,
    }))
}

pub fn render(app: &dyn Host, a: &Value) -> R<Value> {
    let out = PathBuf::from(need_str(a, "out")?);
    let p = app.engine().project();
    let wrap = arg(a, "wrapTail").and_then(Value::as_bool).unwrap_or(false);
    let from = opt_f64(a, "from");
    let to = opt_f64(a, "to");
    let (from, to) = match (wrap, p.loop_range) {
        (true, Some(l)) if l.b > l.a => (from.unwrap_or(l.a), to.unwrap_or(l.b)),
        (true, _) => from.zip(to).ok_or("wrap tail requires a project loop or both `from` and `to`")?,
        (false, range) => (from.unwrap_or(0.0), to.unwrap_or_else(|| range.map_or_else(|| p.song_end(), |l| l.b))),
    };
    let bounce = Bounce { from, to, tail: opt_f64(a, "tail").unwrap_or(2.0), rate: opt_f64(a, "rate").unwrap_or(48000.0) as u32, wrap };
    let wav_bits = arg(a, "wavBits")
        .map(|value| match value.as_f64() {
            Some(24.0) => Ok(24),
            Some(32.0) => Ok(32),
            _ => Err("WAV bit depth must be 24 (PCM) or 32 (float)"),
        })
        .transpose()?;
    let dither = arg(a, "dither").and_then(Value::as_bool).unwrap_or(false);
    if p.clips.iter().any(|c| c.tune.is_some()) {
        // pitch-edited clips are rendered with the same detector the app uses
        app.ensure_pitch_model();
    }
    if arg(a, "stems").and_then(Value::as_bool).unwrap_or(false) {
        if arg(a, "format").and_then(Value::as_str).is_some_and(|f| !f.eq_ignore_ascii_case("wav")) {
            return Err("stems are always WAV; drop --format".into());
        }
        if !out.is_dir()
            && out.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("mp3"))
        {
            return Err("with stems, `out` is a folder for the files, not a .wav/.mp3 file".into());
        }
        let encoding = WavEncoding::parse(wav_bits, dither)?;
        let master = arg(a, "master").and_then(Value::as_bool).unwrap_or(true);
        // a stem export must not start while the piano is still loading (its stems would be silent)
        app.wait_keys()?;
        return render_stems_to_dir(
            app.engine(),
            &out,
            bounce,
            &std::sync::atomic::AtomicBool::new(false),
            master,
            encoding,
            &|_, _, _| {},
        );
    }
    let by_ext = out.extension().and_then(|e| e.to_str()).filter(|e| e.eq_ignore_ascii_case("mp3")).map(|_| "mp3");
    let fmt = ExportFormat::parse(
        arg(a, "format").and_then(Value::as_str).or(by_ext),
        opt_f64(a, "mp3Kbps").map(|k| k as u32),
        wav_bits,
        dither,
    )?;
    render_to_file(app, &out, bounce, &std::sync::atomic::AtomicBool::new(false), fmt)
}

pub fn context(app: &dyn Host, full: bool) -> Value {
    let st = app.engine().state();
    if full {
        return json!({ "revision": st.revision, "project": st.project, "transport": st.transport, "path": st.path, "fxErrors": st.fx_errors });
    }
    let p = &st.project;
    let db = |t: f32| {
        let g = wave_engine::fader_gain(t);
        if g < 1e-6 {
            "-inf".to_string()
        } else {
            format!("{:+.1} dB", 20.0 * g.log10())
        }
    };
    json!({
        "revision": st.revision,
        "path": st.path,
        "dirty": st.dirty,
        "bpm": p.bpm,
        "key": p.key,
        "scale": p.scale,
        "loop": p.loop_range,
        "songEndBeat": p.song_end(),
        "transport": st.transport,
        "tracks": p.tracks.iter().enumerate().map(|(i, t)| json!({
            "index": i, "id": t.id, "name": t.name, "insert": t.insert, "volume": db(t.vol),
            "muted": t.muted, "solo": t.solo, "armed": t.arm,
            "clips": p.clips.iter().filter(|c| c.track as usize == i).count(),
            "takes": p.clips.iter().filter(|c| c.track as usize == i && c.kind == wave_engine::ClipKind::Comp).filter_map(|c| p.comp(c.source)).map(|cp| cp.takes.len()).sum::<usize>(),
        })).collect::<Vec<_>>(),
        "inserts": p.inserts.iter().map(|m| json!({
            "id": m.id, "name": m.name, "volume": db(m.vol), "pan": m.pan, "muted": m.muted, "solo": m.solo,
            "fx": m.fx.iter().map(|f| json!({
                "id": f.id, "type": f.kind, "name": f.plugin.as_ref().map(|x| x.name.clone()).unwrap_or_else(|| catalog::type_name(f.kind).into()),
                "bypass": f.bypass, "preset": f.preset.and_then(|i| catalog::presets(f.kind).get(i as usize).map(|p| p.name)),
                "params": f.params, "error": st.fx_errors.get(&f.id),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "patterns": p.patterns.iter().map(|x| json!({"id": x.id, "name": x.name, "lenBeats": x.len, "swing": x.swing, "notes": x.notes.len()})).collect::<Vec<_>>(),
        "channels": p.channels.iter().map(|c| channel_json(p, c)).collect::<Vec<_>>(),
        "sources": p.sources.iter().map(|s| json!({"id": s.id, "name": s.name, "seconds": s.dur, "path": s.path})).collect::<Vec<_>>(),
        "clips": p.clips.len(),
        "notebook": p.notebook.pages.iter().map(|pg| json!({"id": pg.id, "name": pg.name, "startBar": pg.start_bar, "lines": pg.text.lines().count()})).collect::<Vec<_>>(),
        "nextId": p.next_id,
    })
}

pub fn schema() -> Value {
    json!({
        "automation": "an automation plays through a clip of kind `auto` whose `ref` is the automation id; points t = beats from the automation start, v = 0..1 normalised (log params normalised logarithmically); optional per point, for the segment ending there: tension -1..1 (0 = straight) and shape curve|double|hold|stairs (stairs: tension sets 2..16 steps)",
        "units": "time in beats (quarter notes); pattern len in beats (4 = 1 bar, derived from the notes, at least minLen), note step/len in sixteenths, note v (velocity) 0..1, swing 0..1; fades in seconds; vol = fader 0..1 (0.7937 = 0 dB); clip gain in dB; pan -1..1",
        "patterns": "channels are global (Channel Rack rows, `wave channels`); a pattern note plays channel `ch`; p = MIDI key (absent = the channel's root, the key at which a sampler plays its file unpitched); swing delays notes on odd 16ths by swing × ½ step; the pattern length is derived: the end of the last note rounded up to a bar, at least `minLen` beats (a `len` you send is ignored); when it changes, clips that show exactly one whole pass (offset 0) follow. Easier: `wave pattern --id N --ch C --steps \"x...x...x...x...\"`",
        "ids": "take new ids from context.nextId and count up",
        "stretch": "audio clips only: `stretch` 0.25..4 = how many times longer the clip plays its audio (2 = half speed); `offset` and `len` are beats of the stretched audio, so stretching a clip from s0 to s1 multiplies its len and offset by s1/s0. Pitch is kept (rendered in the background; the clip plays resampled until it is ready, see state `stretching`) unless `resample` is true (speed and pitch change together, like a turntable)",
        "ops": {
            "putClips": {"clips": [{"id": 0, "track": 0, "start": 0, "len": 4, "offset": 0, "kind": "pat|audio|auto|comp", "ref": "pattern, source, automation or comp id", "muted": false, "gain": 0, "fadeIn": 0, "fadeOut": 0, "pan": 0, "norm": false, "rev": false, "stretch": 1, "resample": false}]},
            "removeClips": {"ids": [0]},
            "putTrack": {"track": {"id": 0, "name": "", "color": "#4f8ef7", "muted": false, "solo": false, "arm": false, "vol": 0.7937, "h": 56, "insert": 1}},
            "putInsert": {"insert": {"id": 0, "name": "", "color": "#8b93a1", "vol": 0.7937, "pan": 0, "muted": false, "solo": false, "fx": []}},
            "removeInsert": {"id": 0},
            "putFx": {"insertId": 1, "fx": {"id": 0, "type": "comp", "bypass": false, "params": {"thr": -18}}, "index": null},
            "removeFx": {"fxId": 0},
            "moveFx": {"fxId": 0, "to": 0},
            "putPattern": {"pattern": {"id": 0, "name": "", "color": "#4f8ef7", "minLen": 4, "swing": 0, "notes": [{"ch": "channel id", "step": 0, "len": 1, "p": 36, "v": 1}]}},
            "putChannel": {"channel": {"id": 0, "name": "Kick", "color": "#4f8ef7", "kind": "sample|synth|keys", "source": "source id (kind sample)", "sampler": "kind sample, optional: {start, end (0..1 of the file), reverse, norm, semi, fine, gate (false = one-shot), attack, decay, release (ms), sustain (0..1), cut (0 = none, 1..8)}", "vol": 0.7937, "pan": 0, "mute": false, "solo": false, "insert": "insert id or null (= the clip's track)", "root": 60}, "index": null},
            "removeChannel": {"id": 0},
            "removePattern": {"id": 0},
            "putSource": {"source": "rename/recolour an existing source"},
            "removeSource": {"id": 0},
            "setBpm": {"bpm": 128},
            "setLoop": {"loop": {"a": 0, "b": 16}},
            "putAutomation": {"automation": {"id": 0, "name": "Reverb mix", "color": "#e0a44a", "target": {"kind": "fx|mix|insertVol|insertPan|trackVol|chanVol|chanPan|synth|keys|clipGain|clipPan", "fxId": 0, "chanId": 0, "param": "fx: mix or band:<i>:f|g|q; synth: catalog id; keys: release|dynamics", "id": "insert/track/channel/clip id for vol/pan/gain"}, "points": [{"t": 0, "v": 0.2}, {"t": 8, "v": 0.8, "tension": -0.4}, {"t": 12, "v": 0.5, "shape": "hold"}]}},
            "removeAutomation": {"id": 0},
            "transformNotes": {"pattern": 1, "verb": {"kind": "humanize|quantize|euclidFill|strum|euclidDrums|melody", "timing": 0.3, "velocity": 0.2, "grid": 1, "amount": 1, "scaleAware": false, "ch": 0, "k": 4, "n": 16, "rotate": 0, "vel": 0.7, "width": 0.5, "ramp": 0.05, "voices": [{"ch": 0, "k": 4, "n": 16, "rotate": 0, "vel": 0.7}], "bars": 2, "density": 0.5, "repeat": 1}, "seed": 7, "select": {"kind": "indices|range", "indices": [1, 2], "from": 0, "to": 16, "ch": 0}},
            "setKey": {"key": 2, "scale": 1},
            "putComp": {"comp": {"id": 0, "name": "Comp 1", "xfade": 0.01, "takes": [{"id": 0, "name": "Take 1", "ref": "source id", "start": 0, "len": 8, "offset": 0}], "regions": [{"take": 0, "from": 0, "to": 4, "xfade": 0.02}]}},
            "removeComp": {"id": 0},
            "putNotebook": {"notebook": {"pages": [{"id": 0, "name": "Zwrotka 1", "startBar": 1, "text": "[Zwrotka]\nfirst bar\n\nthird bar"}]}},
        },
        "notebook": "lyrics notebook (text only, never played): pages in tab order; each line of `text` is one bar counted from `startBar` (1-based, bar = 4 beats), an empty line is a bar of rest, a line wrapped in [brackets] is a header and takes no bar. putNotebook replaces all pages; the full text is in `wave context --full`",
        "comps": "take lanes: a clip of kind `comp` plays comp `ref`; takes/regions are in comp beats (comp beat = clip.offset + timeline beat − clip.start); regions sorted, non-overlapping, gaps are silent; xfade in seconds (equal-power crossfade centred on each region start, default 0.01). Easier: `wave takes --track N`, `wave comp --track N --set '[{\"take\":ID,\"from\":B,\"to\":B}]'` (timeline beats)",
        "example": [{"op": "setBpm", "bpm": 140}, {"op": "removeClips", "ids": [3]}],
    })
}

pub fn import(app: &dyn Host, a: &Value) -> R<Value> {
    let paths: Vec<PathBuf> = arg(a, "paths")
        .and_then(Value::as_array)
        .ok_or("`paths` is required")?
        .iter()
        .filter_map(|v| v.as_str().map(PathBuf::from))
        .collect();
    let place = match (opt_f64(a, "track"), opt_f64(a, "beat")) {
        (Some(t), b) => Some((t as u32, b.unwrap_or(0.0))),
        (None, Some(b)) => Some((0, b)),
        _ => None,
    };
    let (s, c) = app.engine().import_audio(&paths, place).map_err(|e| e.to_string())?;
    Ok(json!({ "sourceIds": s, "clipIds": c }))
}

// ---------------------------------------------------------------- channels & patterns

fn channel_json(p: &wave_engine::Project, c: &wave_engine::Channel) -> Value {
    use wave_engine::ChannelSrc;
    let g = c.gain();
    let (kind, what) = match &c.src {
        ChannelSrc::Sample { source, sampler } => (
            "sample",
            json!({ "source": source, "sourceName": p.sources.iter().find(|s| s.id == *source).map(|s| s.name.clone()), "sampler": sampler }),
        ),
        ChannelSrc::Synth { synth } => ("synth", json!({ "preset": synth.preset })),
        ChannelSrc::Keys { keys } => ("keys", json!({ "release": keys.release, "dynamics": keys.dynamics })),
    };
    let mut v = json!({
        "id": c.id, "index": p.channels.iter().position(|x| x.id == c.id), "name": c.name, "kind": kind,
        "volume": if g < 1e-6 { "-inf".to_string() } else { format!("{:+.1} dB", 20.0 * g.log10()) }, "vol": c.vol, "pan": c.pan,
        "mute": c.mute, "solo": c.solo, "insert": c.insert, "root": c.root,
        "notes": p.patterns.iter().map(|pt| pt.notes.iter().filter(|n| n.ch == c.id).count()).sum::<usize>(),
    });
    if let (Value::Object(m), Value::Object(w)) = (&mut v, what) {
        m.extend(w);
    }
    v
}

/// A channel by id, by name (case-insensitive) or by built-in instrument name.
fn find_channel<'a>(p: &'a wave_engine::Project, v: &Value) -> R<&'a wave_engine::Channel> {
    if let Some(id) = as_index(v) {
        return p.channel(id).ok_or(format!("no channel {id} (see `wave channels`)"));
    }
    let s = v.as_str().ok_or("`ch` is a channel id or name")?.to_lowercase();
    p.channels.iter().find(|c| c.name.to_lowercase() == s).ok_or(format!("no channel named `{s}` (see `wave channels`)"))
}

pub fn channels(app: &dyn Host, a: &Value, if_revision: Option<u64>) -> R<Value> {
    use wave_engine::{factory, Channel, ChannelSrc};
    let e = app.engine();
    let mut p = e.project();
    let mut ops = vec![];
    let mut added = None;
    if let Some(k) = arg(a, "add").and_then(Value::as_str) {
        // a sampler playing a standard-library sample: its file name (with or without
        // ".flac"), its display name, or the start of either ("kick" = the first kick)
        let want = k.to_lowercase();
        let stem = |f: &factory::FactoryFile| f.file.trim_end_matches(".flac").to_string();
        let files = factory::files();
        let f = files
            .iter()
            .find(|f| stem(f) == want.trim_end_matches(".flac") || f.name.to_lowercase() == want)
            .or_else(|| files.iter().find(|f| stem(f).starts_with(&want) || f.name.to_lowercase().starts_with(&want)))
            .ok_or(format!("no standard-library sample `{k}`: try kick-808, snare-studio, hat-808-open, bass-808, marimba ... (the browser's Library tab lists them all)"))?;
        // the import adds the project source (or finds the one already there)
        let (srcs, _) = e.import_audio(&[PathBuf::from(factory::lib_path(f.file))], None).map_err(|x| x.to_string())?;
        p = e.project();
        let mut ch = Channel::sampler(p.next_id, f.name, factory::color(f), srcs[0]);
        ch.root = f.root;
        added = Some(ch.id);
        ops.push(Op::PutChannel { channel: ch, index: None });
    }
    if let Some(src) = arg(a, "sample").and_then(as_index) {
        let s = p.sources.iter().find(|s| s.id == src).ok_or(format!("no source {src} (see `wave context`)"))?;
        let id = p.next_id + ops.len() as u64;
        let ch = Channel::sampler(id, &s.name, &s.color, src);
        added = Some(id);
        ops.push(Op::PutChannel { channel: ch, index: None });
    }
    if let Some(name) = arg(a, "synth").and_then(Value::as_str) {
        // a synth channel playing a factory preset ("" = the blank patch)
        let id = p.next_id + ops.len() as u64;
        let synth = if name.is_empty() {
            wave_engine::SynthState::default()
        } else {
            wave_engine::poly::preset_state(name).ok_or(format!("no synth preset `{name}` (see `wave catalog`)"))?
        };
        let bass = wave_engine::poly::presets::all().iter().any(|x| x.name == name && matches!(x.category, "808" | "Bas"));
        let label = if name.is_empty() { "Synth".to_string() } else { name.to_string() };
        let ch = Channel { root: if bass { 36 } else { 60 }, ..Channel::new(id, &label, "#9b7cf8", ChannelSrc::Synth { synth }) };
        added = Some(id);
        ops.push(Op::PutChannel { channel: ch, index: None });
    }
    if arg(a, "keys").and_then(Value::as_bool).unwrap_or(false) {
        // a WAVE Keys channel: the sampled grand piano
        let id = p.next_id + ops.len() as u64;
        let ch = Channel::new(id, "WAVE Keys", "#e0a44a", ChannelSrc::Keys { keys: Default::default() });
        added = Some(id);
        ops.push(Op::PutChannel { channel: ch, index: None });
    }
    if let (Some(v), Some(name)) = (arg(a, "ch"), arg(a, "preset").and_then(Value::as_str)) {
        // load a factory preset into a synth channel
        let mut c = find_channel(&p, v)?.clone();
        if !matches!(c.src, ChannelSrc::Synth { .. }) {
            return Err(format!("channel `{}` is not a synth", c.name));
        }
        c.src = ChannelSrc::Synth {
            synth: wave_engine::poly::preset_state(name).ok_or(format!("no synth preset `{name}` (see `wave catalog`)"))?,
        };
        ops.push(Op::PutChannel { channel: c, index: None });
    }
    if let (Some(v), Some(param)) = (arg(a, "ch"), arg(a, "param").and_then(Value::as_str)) {
        // one synth knob
        let mut c = find_channel(&p, v)?.clone();
        let value = opt_f64(a, "value").ok_or("`value` is required")?;
        match &mut c.src {
            ChannelSrc::Synth { synth } => {
                let i = wave_engine::poly::params::index_of(param)
                    .ok_or(format!("the synth has no parameter `{param}` (see `wave catalog`)"))?;
                synth.params.insert(param.to_string(), wave_engine::poly::params::clamp(i, value) as f64);
                synth.preset = None;
            }
            ChannelSrc::Keys { keys } => match param {
                "release" => keys.release = (value as f32).clamp(wave_engine::keys::RELEASE_MS.0, wave_engine::keys::RELEASE_MS.1),
                "dynamics" => keys.dynamics = (value as f32).clamp(0.0, 100.0),
                _ => return Err(format!("WAVE Keys has no parameter `{param}` (release in ms, dynamics in %)")),
            },
            ChannelSrc::Sample { sampler, .. } => {
                let (v, on) = (value as f32, value != 0.0);
                match param {
                    "start" => sampler.start = v,
                    "end" => sampler.end = v,
                    "reverse" => sampler.reverse = on,
                    "norm" => sampler.norm = on,
                    "semi" => sampler.semi = value.round().clamp(-128.0, 127.0) as i8,
                    "fine" => sampler.fine = v,
                    "gate" => sampler.gate = on,
                    "attack" => sampler.attack = v,
                    "decay" => sampler.decay = v,
                    "sustain" => sampler.sustain = v,
                    "release" => sampler.release = v,
                    "cut" => sampler.cut = value.round().clamp(0.0, 255.0) as u8,
                    _ => return Err(format!("the sampler has no parameter `{param}` (start, end: 0..1; reverse, norm, gate: 0 or 1; semi, fine; attack, decay, release in ms; sustain 0..1; cut 0..8)")),
                }
            }
        }
        ops.push(Op::PutChannel { channel: c, index: None });
    }
    if let Some(v) = arg(a, "remove") {
        ops.push(Op::RemoveChannel { id: find_channel(&p, v)?.id });
    }
    if ops.is_empty() {
        return Ok(json!(p.channels.iter().map(|c| channel_json(&p, c)).collect::<Vec<_>>()));
    }
    let rev = e.apply(&ops, false, if_revision).map_err(|x| x.to_string())?;
    let p = e.project();
    Ok(json!({ "revision": rev, "added": added, "channels": p.channels.iter().map(|c| channel_json(&p, c)).collect::<Vec<_>>() }))
}

/// One character per 16th: `.` empty, `x` full velocity, `1`..`9` = 10..90 %.
fn step_char(v: f32) -> char {
    if v >= 0.95 {
        'x'
    } else {
        char::from_digit((v * 10.0).round().clamp(1.0, 9.0) as u32, 10).unwrap_or('x')
    }
}

/// Parses a step string: `x`/`X` = on (velocity `vel`), `1`..`9` = 10..90 %, `.` `-` `_` `0` = off;
/// spaces and `|` are ignored (for readability: "x... x... | x... x...").
pub fn parse_steps(s: &str, vel: f32) -> R<Vec<(f32, f32)>> {
    let mut out = vec![];
    let mut k = 0;
    for ch in s.chars() {
        match ch {
            ' ' | '|' => continue,
            'x' | 'X' => out.push((k as f32, vel)),
            '1'..='9' => out.push((k as f32, ch.to_digit(10).unwrap() as f32 / 10.0)),
            '.' | '-' | '_' | '0' => {}
            other => return Err(format!("`{other}` in --steps: use x (on), 1-9 (velocity 10-90 %), . (off)")),
        }
        k += 1;
    }
    Ok(out)
}

fn pattern_json(p: &wave_engine::Project, pt: &wave_engine::Pattern) -> Value {
    let steps = (pt.len * 4.0).round().max(1.0) as usize;
    let rows: Vec<Value> = p
        .channels
        .iter()
        .filter(|c| pt.notes.iter().any(|n| n.ch == c.id))
        .map(|c| {
            let mut line = vec!['.'; steps];
            let notes: Vec<_> = pt.notes.iter().filter(|n| n.ch == c.id).collect();
            for n in &notes {
                let i = n.step.round() as usize;
                if i < steps {
                    line[i] = step_char(n.v);
                }
            }
            let s: String = line.chunks(4).map(|g| g.iter().collect::<String>()).collect::<Vec<_>>().join(" ");
            let simple = notes.iter().all(|n| n.len == 1.0 && n.p.is_none() && n.step.fract() == 0.0);
            let mut row = json!({ "ch": c.id, "name": c.name, "steps": s });
            if !simple {
                row["notes"] = json!(notes
                    .iter()
                    .map(|n| json!({ "step": n.step, "len": n.len, "p": n.p.unwrap_or(c.root), "v": n.v }))
                    .collect::<Vec<_>>());
            }
            row
        })
        .collect();
    json!({ "id": pt.id, "name": pt.name, "color": pt.color, "bars": pt.len / 4.0, "lenBeats": pt.len, "minBars": pt.min_len / 4.0, "swing": (pt.swing * 100.0).round(), "rows": rows })
}

/// `wave pattern`: show a pattern as step strings, or edit it (one undo step).
pub fn pattern(app: &dyn Host, a: &Value, if_revision: Option<u64>) -> R<Value> {
    use wave_engine::{Note, Pattern};
    let e = app.engine();
    let p = e.project();
    let new = arg(a, "new").and_then(Value::as_bool).unwrap_or(false);
    let mut pt = if new {
        Pattern {
            id: p.next_id,
            name: format!("Pattern {}", p.patterns.len() + 1),
            color: "#4f8ef7".into(),
            len: 4.0,
            min_len: 4.0,
            notes: vec![],
            swing: 0.0,
        }
    } else {
        let id = match arg(a, "id").and_then(as_index) {
            Some(id) => id,
            None if p.patterns.len() == 1 => p.patterns[0].id,
            None => {
                return Err(format!(
                    "which pattern? pass --id ({})",
                    p.patterns.iter().map(|x| format!("{} {}", x.id, x.name)).collect::<Vec<_>>().join(", ")
                ))
            }
        };
        p.patterns.iter().find(|x| x.id == id).cloned().ok_or(format!("no pattern {id}"))?
    };
    let before = pt.clone();
    if let Some(n) = arg(a, "name").and_then(Value::as_str) {
        pt.name = n.into();
    }
    if let Some(c) = arg(a, "color").and_then(Value::as_str) {
        pt.color = c.into();
    }
    if let Some(b) = opt_f64(a, "bars") {
        // the shortest the pattern may be: it still grows with notes past it
        if !(1.0..=wave_engine::TOTAL_BEATS / 4.0).contains(&b) || b.fract() != 0.0 {
            return Err(format!(
                "--bars is a whole number 1..{} (the pattern still grows to fit its notes)",
                wave_engine::TOTAL_BEATS / 4.0
            ));
        }
        pt.min_len = b * 4.0;
    }
    if let Some(s) = opt_f64(a, "swing") {
        if !(0.0..=100.0).contains(&s) {
            return Err("--swing is 0..100 (%)".into());
        }
        pt.swing = (s / 100.0) as f32;
    }
    let steps = arg(a, "steps").and_then(Value::as_str);
    let notes = arg(a, "notes");
    if steps.is_some() || notes.is_some() || arg(a, "clear").and_then(Value::as_bool).unwrap_or(false) {
        let ch = find_channel(&p, arg(a, "ch").ok_or("--ch (channel id or name) is required with --steps / --notes / --clear")?)?.id;
        pt.notes.retain(|n| n.ch != ch);
        if let Some(s) = steps {
            let vel = opt_f64(a, "vel").unwrap_or(1.0) as f32;
            if !(0.0..=1.0).contains(&vel) {
                return Err("--vel is 0..1".into());
            }
            pt.notes.extend(parse_steps(s, vel)?.into_iter().map(|(k, v)| Note { v, ..Note::new(ch, k, 1.0, None) }));
        }
        if let Some(ns) = notes {
            let list = ns.as_array().ok_or("--notes is a JSON array of {step, len, p?, v?}")?;
            for n in list {
                let step = opt_f64(n, "step").ok_or("each note needs `step` (16ths)")? as f32;
                let len = opt_f64(n, "len").unwrap_or(1.0) as f32;
                let key = arg(n, "p").and_then(as_index).map(|k| k.min(127) as u8);
                let v = opt_f64(n, "v").unwrap_or(1.0) as f32;
                pt.notes.push(Note { v, ..Note::new(ch, step, len, key) });
            }
        }
    }
    if !new && pt == before {
        return Ok(pattern_json(&p, &pt));
    }
    let rev = e.apply(&[Op::PutPattern { pattern: pt.clone() }], false, if_revision).map_err(|x| x.to_string())?;
    let p = e.project();
    let pt = p.patterns.iter().find(|x| x.id == pt.id).ok_or("the pattern vanished")?;
    let mut v = pattern_json(&p, pt);
    v["revision"] = json!(rev);
    Ok(v)
}

// ---------------------------------------------------------------- takes & comps

/// The comp clip a takes/comp command means: `clip` if given, else the only one on `track`.
fn comp_clip(p: &wave_engine::Project, a: &Value) -> R<wave_engine::Clip> {
    use wave_engine::ClipKind;
    if let Some(id) = arg(a, "clip").and_then(as_index) {
        return p.clips.iter().find(|c| c.id == id && c.kind == ClipKind::Comp).cloned().ok_or(format!("clip {id} is not a comp clip"));
    }
    let track = need_u64(a, "track")? as u32;
    if track as usize >= p.tracks.len() {
        return Err(format!("no track {track} (tracks are 0..{})", p.tracks.len().saturating_sub(1)));
    }
    let cs: Vec<_> = p.clips.iter().filter(|c| c.track == track && c.kind == ClipKind::Comp).collect();
    match cs.len() {
        0 => Err(format!("track {track} has no takes (record with a loop set, or record over an existing clip)")),
        1 => Ok(cs[0].clone()),
        _ => Err(format!(
            "track {track} has several comp clips; pick one with --clip ({})",
            cs.iter().map(|c| c.id.to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Takes and comp of the comp clips on a track, in timeline beats.
pub fn takes(app: &dyn Host, a: &Value) -> R<Value> {
    let p = app.engine().project();
    let track = need_u64(a, "track")? as u32;
    let audition = app.engine().state().audition;
    let out: Vec<Value> = p
        .clips
        .iter()
        .filter(|c| c.track == track && c.kind == wave_engine::ClipKind::Comp)
        .filter_map(|c| {
            let cp = p.comp(c.source)?;
            let tl = |b: f64| b - c.offset + c.start;
            Some(json!({
                "clipId": c.id, "compId": cp.id, "name": cp.name, "start": c.start, "len": c.len,
                "xfadeMs": cp.xfade * 1000.0, "audition": audition.get(&cp.id),
                "takes": cp.takes.iter().map(|t| json!({
                    "id": t.id, "name": t.name, "source": t.source, "start": tl(t.start), "len": t.len,
                    "usedBeats": cp.regions.iter().filter(|r| r.take == t.id).map(|r| r.to - r.from).sum::<f64>(),
                })).collect::<Vec<_>>(),
                "regions": cp.regions.iter().map(|r| json!({
                    "take": r.take, "from": tl(r.from), "to": tl(r.to), "xfadeMs": r.xfade.map(|x| x * 1000.0),
                })).collect::<Vec<_>>(),
            }))
        })
        .collect();
    if out.is_empty() {
        return Err(format!("track {track} has no takes"));
    }
    Ok(json!(out))
}

/// Edits the comp of a comp clip. Times are timeline beats, crossfades milliseconds.
/// `regions` replaces the comp, `take` uses one whole take, `xfadeMs` sets the default
/// crossfade, `removeUnused` drops takes the comp does not play. One undo step.
pub fn comp(app: &dyn Host, a: &Value, if_revision: Option<u64>) -> R<Value> {
    let p = app.engine().project();
    let clip = comp_clip(&p, a)?;
    let mut cp = p.comp(clip.source).cloned().ok_or("the clip's comp is missing")?;
    let local = |b: f64| b - clip.start + clip.offset;
    let ms = |v: &Value, k: &str| -> R<Option<f32>> {
        match arg(v, k) {
            None => Ok(None),
            Some(x) => x.as_f64().map(|m| Some((m / 1000.0) as f32)).ok_or(format!("`{k}` must be a number of milliseconds")),
        }
    };
    let mut did = vec![];
    if let Some(rs) = arg(a, "regions") {
        let rs = rs.as_array().ok_or("`regions` must be an array of {take, from, to, xfadeMs?}")?;
        let mut regions = vec![];
        for r in rs {
            let take = need_u64(r, "take")?;
            let (from, to) = (opt_f64(r, "from").ok_or("each region needs `from`")?, opt_f64(r, "to").ok_or("each region needs `to`")?);
            regions.push(wave_engine::CompRegion { take, from: local(from), to: local(to), xfade: ms(r, "xfadeMs")? });
        }
        regions.sort_by(|x, y| x.from.total_cmp(&y.from));
        // a range wider than its take plays the take only
        for r in regions.iter_mut() {
            if let Some(t) = cp.take(r.take) {
                r.from = r.from.max(t.start);
                r.to = r.to.min(t.end());
            }
        }
        regions.retain(|r| r.to > r.from + 1e-9);
        cp.regions = regions;
        did.push("regions");
    }
    if let Some(t) = arg(a, "take").and_then(as_index) {
        let tk = cp.take(t).cloned().ok_or(format!("the comp has no take {t}"))?;
        cp.regions.clear();
        cp.write(t, tk.start, tk.end());
        did.push("take");
    }
    if let Some(x) = ms(a, "xfadeMs")? {
        cp.xfade = x;
        did.push("xfade");
    }
    if arg(a, "removeUnused").and_then(Value::as_bool).unwrap_or(false) {
        let used: Vec<u64> = cp.regions.iter().map(|r| r.take).collect();
        let n = cp.takes.len();
        cp.takes.retain(|t| used.contains(&t.id));
        if cp.takes.is_empty() {
            return Err("the comp plays no take; keep at least one".into());
        }
        did.push(if cp.takes.len() < n { "removeUnused" } else { "nothingUnused" });
    }
    if did.is_empty() {
        return Err("nothing to do: pass regions, take, xfadeMs or removeUnused".into());
    }
    let rev = app.engine().apply(&[Op::PutComp { comp: cp }], false, if_revision).map_err(|e| e.to_string())?;
    Ok(json!({ "revision": rev, "clipId": clip.id, "changed": did }))
}

/// Switches a clip to Tune to Reference (`reference` = source id), or back to
/// manual editing with `off`. Unset parameters keep their current values.
pub fn tune_ref(app: &dyn Host, a: &Value, if_revision: Option<u64>) -> R<u64> {
    let p = app.engine().project();
    let cid = need_u64(a, "clipId")?;
    let mut clip = p.clips.iter().find(|c| c.id == cid).cloned().ok_or(format!("no clip {cid}"))?;
    let mut tune = clip.tune.take().unwrap_or_default();
    if arg(a, "off").and_then(Value::as_bool).unwrap_or(false) {
        tune.reference = None;
    } else {
        let prev = tune.reference.clone();
        // `melodyChannel`: the notes that channel plays over the clip become the reference (a
        // copy, collected again by naming the channel again); `reference`: a recording
        let melody = match arg(a, "melodyChannel").and_then(as_index) {
            Some(ch) => Some(app.engine().melody_reference(cid, ch, None).map_err(|e| e.to_string())?.0),
            None if arg(a, "reference").is_some() => None,
            None => prev.as_ref().and_then(|r| r.melody.clone()),
        };
        let source = match &melody {
            Some(_) => 0,
            None => arg(a, "reference")
                .and_then(as_index)
                .or(prev.as_ref().filter(|r| r.melody.is_none()).map(|r| r.source))
                .ok_or("`reference` (source id) or `melodyChannel` is required")?,
        };
        let unit = |k: &str, d: f32| -> R<f32> {
            match opt_f64(a, k) {
                Some(v) if (0.0..=100.0).contains(&v) => Ok(v as f32 / 100.0),
                Some(_) => Err(format!("`{k}` is a percentage 0..100")),
                None => Ok(d),
            }
        };
        let base = prev.unwrap_or(RefTune {
            source,
            correction: 1.0,
            vibrato: 1.0,
            influence: 1.0,
            octave: None,
            key: 0,
            scale: 0,
            speed: None,
            melody: None,
        });
        let octave = match arg(a, "octave") {
            Some(Value::String(s)) if s == "auto" => None,
            Some(v) => Some(v.as_i64().ok_or("`octave` is auto or a whole number of semitones")? as i32),
            None => base.octave,
        };
        let speed = match opt_f64(a, "speed") {
            Some(v) if (0.0..=1000.0).contains(&v) => Some(v as f32),
            Some(_) => return Err("`speed` is 0..1000 ms".into()),
            None => base.speed,
        };
        tune.reference = Some(RefTune {
            source,
            correction: unit("correction", base.correction)?,
            vibrato: unit("vibrato", base.vibrato)?,
            // a melody gives every note's pitch: no scale share
            influence: if melody.is_some() { 1.0 } else { unit("influence", base.influence)? },
            octave,
            key: arg(a, "key").and_then(as_index).map(|k| k as u32).unwrap_or(base.key),
            scale: arg(a, "scale").and_then(as_index).map(|k| k as u32).unwrap_or(base.scale),
            speed,
            melody,
        });
    }
    clip.tune = tune.is_active().then_some(tune);
    app.engine().apply(&[Op::PutClips { clips: vec![clip] }], false, if_revision).map_err(|e| e.to_string())
}

/// The CLI entry: one request → JSON data.
pub fn control(app: &dyn Host, cmd: &str, a: &Value, if_revision: Option<u64>) -> R<Value> {
    let e = app.engine();
    let ok_rev = |r: u64| json!({ "revision": r });
    if !app.live() && LIVE_ONLY.contains(&cmd) {
        return Err(live_only_error(cmd));
    }
    match cmd {
        "pitch_analyze" | "pitch_target" | "tune_ref" | "auto_vocal_plan" | "f0_model" => app.ensure_pitch_model(),
        "vst3_params" | "vst3_set_param" => app.ensure_live_plugins(),
        _ => {}
    }
    match cmd {
        "status" => {
            let st = e.state();
            Ok(
                json!({ "revision": st.revision, "transport": st.transport, "path": st.path, "dirty": st.dirty, "mode": if app.live() { "app" } else { "file" }, "audio": app.audio_json(), "cli": app.cli_json() }),
            )
        }
        "context" => Ok(context(app, arg(a, "full").and_then(Value::as_bool).unwrap_or(false))),
        "catalog" => Ok(
            json!({ "effects": catalog::catalog(), "commands": ["status","context","catalog","schema","apply","undo","redo","play","stop","render","import","add_fx","set_param","load_preset","save","open","pitch_analyze","pitch_target","melody_channels","melody_reference","tune_ref","f0_model","music_analyze","auto_vocal_plan","bake_rider","make_unique","takes","comp","flatten_comp","comp_audition","channels","pattern","pattern_play","note_audition","vst3_scan","vst3_add","vst3_params","vst3_set_param"] }),
        ),
        "schema" => Ok(schema()),
        "apply" => {
            let ops: Vec<Op> =
                serde_json::from_value(arg(a, "ops").cloned().unwrap_or(json!([]))).map_err(|er| format!("BAD_OPS: {er}"))?;
            e.apply(&ops, false, if_revision).map(ok_rev).map_err(|er| er.to_string())
        }
        "undo" => Ok(json!({ "undone": e.undo() })),
        "redo" => Ok(json!({ "redone": e.redo() })),
        "play" => {
            e.play(opt_f64(a, "from").unwrap_or(e.state().transport.start_beat));
            Ok(json!({ "playing": true }))
        }
        "stop" => {
            e.stop();
            Ok(json!({ "playing": false }))
        }
        "render" => render(app, a),
        "import" => import(app, a),
        "add_fx" => {
            let kind = parse_kind(need_str(a, "type")?)?;
            let (id, rev) = add_fx(app, need_u64(a, "insertId")?, kind, arg(a, "preset").and_then(as_index).map(|x| x as usize), None)?;
            Ok(json!({ "fxId": id, "revision": rev }))
        }
        "set_param" => {
            set_param(app, need_u64(a, "fxId")?, need_str(a, "param")?, opt_f64(a, "value").ok_or("`value` is required")?, if_revision)
                .map(ok_rev)
        }
        "load_preset" => preset(app, need_u64(a, "fxId")?, need_u64(a, "index")? as usize).map(ok_rev),
        "save" => {
            let path = match arg(a, "path").and_then(Value::as_str) {
                Some(p) => PathBuf::from(p),
                None => e.state().path.map(PathBuf::from).ok_or("the project has never been saved; pass --path")?,
            };
            e.save(&path).map_err(|x| x.to_string())?;
            Ok(json!({ "path": path.display().to_string() }))
        }
        "open" => {
            let w = e.open(&PathBuf::from(need_str(a, "path")?)).map_err(|x| x.to_string())?;
            Ok(json!({ "warnings": w }))
        }
        "takes" => takes(app, a),
        "channels" => channels(app, a, if_revision),
        "pattern" => pattern(app, a, if_revision),
        "pattern_play" => {
            let id = arg(a, "id").and_then(as_index);
            e.pattern_play(id).map_err(|x| x.to_string())?;
            Ok(json!({ "patternPlaying": id }))
        }
        "note_audition" => {
            let p = e.project();
            let ch = find_channel(&p, arg(a, "ch").ok_or("`ch` is required")?)?.id;
            e.note_audition(
                ch,
                arg(a, "key").and_then(as_index).map(|k| k.min(127) as u8),
                opt_f64(a, "vel").unwrap_or(0.8) as f32,
                arg(a, "pattern").and_then(as_index),
            )
            .map_err(|x| x.to_string())?;
            Ok(json!({}))
        }
        "comp" => comp(app, a, if_revision),
        "make_unique" => {
            let ids: Vec<u64> = arg(a, "clipIds")
                .and_then(Value::as_array)
                .ok_or("`clipIds` (array of clip ids) is required")?
                .iter()
                .filter_map(as_index)
                .collect();
            let r = e.make_unique_audio(&ids).map_err(|x| x.to_string())?;
            Ok(json!({ "sourceId": r.source_id, "track": r.track, "insertId": r.insert_id, "revision": e.revision() }))
        }
        "flatten_comp" => {
            let clip = comp_clip(&e.project(), a)?;
            let id = e.flatten_comp(clip.id).map_err(|x| x.to_string())?;
            Ok(json!({ "clipId": clip.id, "sourceId": id, "revision": e.revision() }))
        }
        "comp_audition" => {
            let clip = comp_clip(&e.project(), a)?;
            e.audition(clip.source, arg(a, "take").and_then(as_index)).map_err(|x| x.to_string())?;
            Ok(json!({ "compId": clip.source, "audition": e.state().audition.get(&clip.source) }))
        }
        "pitch_analyze" => {
            let an = e.pitch_analysis(need_u64(a, "clipId")?).map_err(|x| x.to_string())?;
            Ok(json!({ "rate": an.rate, "backend": an.backend, "notes": an.notes }))
        }
        "pitch_target" => {
            let params = match arg(a, "reference") {
                Some(v) => Some(serde_json::from_value::<RefTune>(v.clone()).map_err(|er| format!("BAD_ARGS: reference: {er}"))?),
                None => None,
            };
            let t = e.pitch_target(need_u64(a, "clipId")?, params).map_err(|x| x.to_string())?;
            if arg(a, "full").and_then(Value::as_bool).unwrap_or(false) {
                return Ok(json!(&*t));
            }
            Ok(
                json!({ "score": t.score, "octave": t.octave, "reason": t.reason, "start": t.start as f64 / t.rate as f64, "notes": t.notes }),
            )
        }
        "melody_channels" => melody_channels(e, need_u64(a, "clipId")?),
        "melody_reference" => {
            let span = match arg(a, "span") {
                Some(Value::Null) | None => None,
                Some(v) => Some(serde_json::from_value::<[f64; 2]>(v.clone()).map_err(|er| format!("BAD_ARGS: span: {er}"))?),
            };
            melody_reference(e, need_u64(a, "clipId")?, need_u64(a, "channel")?, span)
        }
        "tune_ref" => tune_ref(app, a, if_revision).map(ok_rev),
        "f0_model" => Ok(json!(crate::models::status(e, app.model_error()))),
        "music_analyze" => {
            let cid = arg(a, "clipId").and_then(as_index);
            let ex = arg(a, "excludeFx").and_then(as_index);
            let an = e.music_analysis(cid, ex).map_err(|x| x.to_string())?;
            Ok(json!(an))
        }
        "auto_vocal_plan" => {
            let cid = need_u64(a, "clipId")?;
            let beat = arg(a, "beatClipId").and_then(as_index);
            let style = match arg(a, "style").and_then(Value::as_str) {
                Some("hard") => wave_engine::autovocal::Style::Hard,
                _ => wave_engine::autovocal::Style::Natural,
            };
            let bake = arg(a, "bakeRider").and_then(Value::as_bool).unwrap_or(false);
            let plan = e.auto_vocal_plan(cid, beat, style, bake).map_err(|x| x.to_string())?;
            Ok(json!(plan))
        }
        "bake_rider" => {
            let cid = need_u64(a, "clipId")?;
            let params: std::collections::BTreeMap<String, f64> = match arg(a, "params") {
                Some(v) => serde_json::from_value(v.clone()).map_err(|er| format!("BAD_ARGS: params: {er}"))?,
                None => Default::default(),
            };
            let (source, clip) = e.bake_rider(cid, &params).map_err(|x| x.to_string())?;
            Ok(json!({ "clipId": clip, "sourceId": source, "revision": e.revision() }))
        }
        "vst3_scan" => Ok(json!(app.plugins().scan(arg(a, "rescan").and_then(Value::as_bool).unwrap_or(false)))),
        "vst3_add" => {
            let (id, rev) = vst3_add(app, need_u64(a, "insertId")?, need_str(a, "path")?, need_str(a, "cid")?, None)?;
            Ok(json!({ "fxId": id, "revision": rev }))
        }
        "vst3_params" => {
            let q = arg(a, "query").and_then(Value::as_str).map(str::to_lowercase);
            let ps = app.plugins().params(need_u64(a, "fxId")?)?;
            Ok(json!(ps.into_iter().filter(|p| q.as_ref().map(|q| p.name.to_lowercase().contains(q)).unwrap_or(true)).collect::<Vec<_>>()))
        }
        "vst3_set_param" => {
            app.plugins().set_param(need_u64(a, "fxId")?, need_u64(a, "id")? as u32, opt_f64(a, "value").ok_or("`value` is required")?)?;
            Ok(json!({}))
        }
        other => Err(format!("UNKNOWN_COMMAND: `{other}` (see `wave catalog`)")),
    }
}

/// `melody_channels`: the channels whose notes play over an audio clip, best fit to the
/// singing first: `[{channel, name, notes, fit}]` (`fit` 0..1).
pub fn melody_channels(e: &wave_engine::Engine, clip_id: u64) -> Result<Value, String> {
    let list = e.melody_channels(clip_id).map_err(|x| x.to_string())?;
    let p = e.project();
    Ok(json!(list
        .into_iter()
        .map(|(id, n, fit)| json!({ "channel": id, "name": p.channels.iter().find(|c| c.id == id).map(|c| c.name.clone()).unwrap_or_default(), "notes": n, "fit": fit }))
        .collect::<Vec<_>>()))
}

/// `melody_reference`: the notes of `channel` over an audio clip as a melody reference
/// (`span`: seconds of the source) — `{melody: {channel, span?, notes}, fit}`.
pub fn melody_reference(e: &wave_engine::Engine, clip_id: u64, channel: u64, span: Option<[f64; 2]>) -> Result<Value, String> {
    let (m, fit) = e.melody_reference(clip_id, channel, span).map_err(|x| x.to_string())?;
    Ok(json!({ "melody": m, "fit": fit }))
}

#[cfg(test)]
mod tests {
    use super::{parse_steps, render_stems_to_dir, safe_file_stem, unique_file_stems, Bounce, ExportFormat, WavEncoding};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use wave_engine::{Engine, NoPlugins, Project};

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wave-stems-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    fn sketch() -> Arc<Engine> {
        Engine::new(Project::sketch(), Arc::new(NoPlugins))
    }

    fn bounce(to: f64) -> Bounce {
        Bounce { from: 0.0, to, tail: 0.0, rate: 48_000, wrap: false }
    }

    #[test]
    fn wav_format_defaults_and_dither_validation() {
        assert_eq!(ExportFormat::parse(None, None, None, false).unwrap(), ExportFormat::Wav(WavEncoding::Float32));
        assert_eq!(ExportFormat::parse(Some("wav"), None, Some(24), true).unwrap(), ExportFormat::Wav(WavEncoding::Pcm24 { dither: true }));
        assert!(ExportFormat::parse(Some("wav"), None, Some(32), true).is_err());
        assert!(ExportFormat::parse(Some("wav"), None, Some(16), false).is_err());
        assert!(ExportFormat::parse(Some("mp3"), None, Some(24), false).is_err());
        assert!(ExportFormat::parse(Some("mp3"), None, None, true).is_err());
    }

    #[test]
    fn stem_files_are_named_after_the_inserts_and_are_all_equally_long() {
        let dir = tmp("ok");
        let e = sketch();
        let seen = std::sync::Mutex::new(vec![]);
        let r = render_stems_to_dir(&e, &dir, bounce(16.0), &AtomicBool::new(false), true, WavEncoding::Float32, &|d, t, n| {
            seen.lock().unwrap().push((d, t, n.to_string()))
        })
        .unwrap();
        assert_eq!(names(&dir), ["Bass.wav", "Drums.wav", "Hats.wav", "Master.wav"], "no .part file is left");
        // 32-bit float stereo: 8 bytes a frame plus a small header, the same in every file
        let data = (16.0 * 60.0 / 128.0 * 48_000.0) as u64 * 8;
        let lens: Vec<u64> = names(&dir).iter().map(|n| std::fs::metadata(dir.join(n)).unwrap().len()).collect();
        assert!(lens.iter().all(|l| *l == lens[0] && (data..data + 200).contains(l)), "{lens:?} vs {data}");
        assert_eq!(r["files"].as_array().unwrap().len(), 4);
        assert_eq!(r["files"][0]["file"], "Master.wav");
        assert_eq!(r["files"][0]["master"], true);
        assert_eq!(r["skipped"].as_array().unwrap().len(), 0);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.first().unwrap(), &(0, 4, "Master".to_string()));
        assert_eq!(seen.last().unwrap().0, 4, "the last call reports everything done");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_master_file_is_optional() {
        let dir = tmp("nomaster");
        let r = render_stems_to_dir(
            &sketch(),
            &dir,
            bounce(16.0),
            &AtomicBool::new(false),
            false,
            WavEncoding::Pcm24 { dither: false },
            &|_, _, _| {},
        )
        .unwrap();
        assert_eq!(names(&dir), ["Bass.wav", "Drums.wav", "Hats.wav"]);
        let bytes = std::fs::read(dir.join("Bass.wav")).unwrap();
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 24);
        assert!(r["files"].as_array().unwrap().iter().all(|f| f["master"] == false));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stem_that_renders_silent_gets_no_file_and_is_reported() {
        // the hats only play from beat 8, so a bounce of beats 0..4 has nothing in them
        let dir = tmp("silent");
        let r =
            render_stems_to_dir(&sketch(), &dir, bounce(4.0), &AtomicBool::new(false), false, WavEncoding::Float32, &|_, _, _| {}).unwrap();
        assert_eq!(names(&dir), ["Bass.wav", "Drums.wav"]);
        assert_eq!(r["skipped"], serde_json::json!(["Hats"]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancelling_leaves_nothing_behind() {
        let dir = tmp("cancel");
        let cancel = AtomicBool::new(false);
        // cancel from the progress callback once the second file has started: one file is already on disk as .part
        let r = render_stems_to_dir(&sketch(), &dir, bounce(16.0), &cancel, true, WavEncoding::Float32, &|d, _, _| {
            if d == 2 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        assert_eq!(r.unwrap_err(), "cancelled");
        assert!(names(&dir).is_empty(), "left: {:?}", names(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nothing_to_export_is_an_error_not_an_empty_folder() {
        let mut p = Project::sketch();
        for t in p.tracks.iter_mut() {
            t.muted = true;
        }
        let dir = tmp("none");
        let e = Engine::new(p, Arc::new(NoPlugins));
        let err =
            render_stems_to_dir(&e, &dir, bounce(8.0), &AtomicBool::new(false), false, WavEncoding::Float32, &|_, _, _| {}).unwrap_err();
        assert!(err.contains("nothing to export"), "{err}");
        assert!(!dir.exists() || names(&dir).is_empty());
        // with the master asked for, the (silent) mix is still written: it is what the song sounds like
        let r = render_stems_to_dir(&e, &dir, bounce(8.0), &AtomicBool::new(false), true, WavEncoding::Float32, &|_, _, _| {}).unwrap();
        assert_eq!(names(&dir), ["Master.wav"]);
        assert_eq!(r["files"][0]["stats"]["silent"], true);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unwritable_folder_fails_cleanly() {
        let blocker = tmp("blocker");
        std::fs::write(&blocker, b"a file where the folder should go").unwrap();
        let err = render_stems_to_dir(
            &sketch(),
            &blocker.join("stems"),
            bounce(8.0),
            &AtomicBool::new(false),
            true,
            WavEncoding::Float32,
            &|_, _, _| {},
        )
        .unwrap_err();
        assert!(err.contains("cannot create"), "{err}");
        std::fs::remove_file(&blocker).ok();
    }

    #[test]
    fn file_names_are_safe_and_unique() {
        assert_eq!(safe_file_stem("Kick / Snare: 2"), "Kick _ Snare_ 2");
        assert_eq!(safe_file_stem("  Lead...  "), "Lead");
        assert_eq!(safe_file_stem(""), "Insert");
        assert_eq!(safe_file_stem("???"), "___");
        assert_eq!(safe_file_stem("con"), "_con");
        assert_eq!(safe_file_stem("NUL.txt"), "_NUL.txt");
        assert_eq!(safe_file_stem("COM1"), "_COM1");
        assert_eq!(safe_file_stem("COM0"), "COM0");
        assert_eq!(safe_file_stem("Wokal główny"), "Wokal główny");
        assert_eq!(safe_file_stem(&"x".repeat(300)).len(), 100);
        let n = |v: &[&str]| unique_file_stems(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(n(&["Bass", "bass", "BASS", "Bass (2)"]), ["Bass", "bass (2)", "BASS (3)", "Bass (2) (2)"]);
        assert_eq!(n(&["Master", "Master"]), ["Master", "Master (2)"]);
    }

    #[test]
    fn step_strings() {
        assert_eq!(parse_steps("x... x... | x.5. ..9.", 1.0).unwrap(), vec![(0.0, 1.0), (4.0, 1.0), (8.0, 1.0), (10.0, 0.5), (14.0, 0.9)]);
        assert_eq!(parse_steps("X-X_", 0.6).unwrap(), vec![(0.0, 0.6), (2.0, 0.6)]);
        assert!(parse_steps("x?x", 1.0).is_err());
        assert!(parse_steps("", 1.0).unwrap().is_empty());
    }
}
