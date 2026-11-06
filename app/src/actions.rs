//! Everything the GUI and the CLI can ask for, implemented once.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use wave_engine::catalog::{self, load_preset, new_fx};
use wave_engine::{FxKind, Op, PluginRef};

use crate::App;

pub type R<T> = Result<T, String>;

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
    serde_json::from_value(json!(s)).map_err(|_| format!("unknown effect type `{s}` (eq, comp, sat, delay, reverb, limit)"))
}

pub fn add_fx(app: &App, insert_id: u64, kind: FxKind, preset: Option<usize>, index: Option<usize>) -> R<(u64, u64)> {
    if kind == FxKind::Vst3 {
        return Err("use vst3_add for plugins".into());
    }
    let p = app.engine.project();
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
    let rev = app.engine.apply(&[Op::PutFx { insert_id, fx, index }], false, None).map_err(|e| e.to_string())?;
    Ok((id, rev))
}

pub fn set_param(app: &App, fx_id: u64, param: &str, value: f64, if_revision: Option<u64>) -> R<u64> {
    let p = app.engine.project();
    let (m, f) = p.find_fx(fx_id).ok_or(format!("no effect {fx_id}"))?;
    let def = catalog::params(f.kind).iter().find(|d| d.id == param).ok_or_else(|| {
        format!("{} has no parameter `{param}` (has: {})", catalog::type_name(f.kind), catalog::params(f.kind).iter().map(|d| d.id).collect::<Vec<_>>().join(", "))
    })?;
    if !(def.min..=def.max).contains(&value) {
        return Err(format!("{param} must be within {}..{} {}", def.min, def.max, def.unit));
    }
    let mut f = f.clone();
    f.params.insert(param.into(), value);
    app.engine.apply(&[Op::PutFx { insert_id: m.id, fx: f, index: None }], false, if_revision).map_err(|e| e.to_string())
}

pub fn preset(app: &App, fx_id: u64, index: usize) -> R<u64> {
    let p = app.engine.project();
    let (m, f) = p.find_fx(fx_id).ok_or(format!("no effect {fx_id}"))?;
    let mut f = f.clone();
    if !load_preset(&mut f, index, p.bpm) {
        return Err(format!("no preset {index}"));
    }
    app.engine.apply(&[Op::PutFx { insert_id: m.id, fx: f, index: None }], false, None).map_err(|e| e.to_string())
}

pub fn vst3_add(app: &App, insert_id: u64, path: &str, cid: &str, index: Option<usize>) -> R<(u64, u64)> {
    let p = app.engine.project();
    if p.insert(insert_id).is_none() {
        return Err(format!("no insert {insert_id}"));
    }
    let (name, vendor) = app.plugins.info(path, cid).ok_or("that plugin is not in the scan results; run a VST3 scan first")?;
    let id = p.next_id;
    let fx = wave_engine::Fx {
        id,
        kind: FxKind::Vst3,
        bypass: false,
        preset: None,
        params: Default::default(),
        bands: None,
        plugin: Some(PluginRef { format: "vst3".into(), path: path.into(), cid: cid.into(), name, vendor, state: None }),
    };
    let rev = app.engine.apply(&[Op::PutFx { insert_id, fx, index }], false, None).map_err(|e| e.to_string())?;
    if let Some(err) = app.engine.state().fx_errors.get(&id) {
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

pub fn render(app: &App, a: &Value) -> R<Value> {
    let out = PathBuf::from(need_str(a, "out")?);
    let p = app.engine.project();
    let from = opt_f64(a, "from").unwrap_or(0.0);
    let to = opt_f64(a, "to").unwrap_or_else(|| p.loop_range.map(|l| l.b).unwrap_or(p.song_end()));
    let tail = opt_f64(a, "tail").unwrap_or(2.0);
    let rate = opt_f64(a, "rate").unwrap_or(48000.0) as u32;
    if to <= from {
        return Err("`to` must be after `from`".into());
    }
    let audio = app.engine.render(from, to, tail, rate).map_err(|e| e.to_string())?;
    wave_engine::sources::write_wav(&out, &audio, rate)?;
    Ok(json!({ "out": out.display().to_string(), "fromBeat": from, "toBeat": to, "stats": stats(&audio, rate) }))
}

pub fn context(app: &App, full: bool) -> Value {
    let st = app.engine.state();
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
        "loop": p.loop_range,
        "songEndBeat": p.song_end(),
        "transport": st.transport,
        "tracks": p.tracks.iter().enumerate().map(|(i, t)| json!({
            "index": i, "id": t.id, "name": t.name, "insert": t.insert, "volume": db(t.vol),
            "muted": t.muted, "solo": t.solo, "armed": t.arm,
            "clips": p.clips.iter().filter(|c| c.track as usize == i).count(),
        })).collect::<Vec<_>>(),
        "inserts": p.inserts.iter().map(|m| json!({
            "id": m.id, "name": m.name, "volume": db(m.vol), "pan": m.pan, "muted": m.muted, "solo": m.solo,
            "fx": m.fx.iter().map(|f| json!({
                "id": f.id, "type": f.kind, "name": f.plugin.as_ref().map(|x| x.name.clone()).unwrap_or_else(|| catalog::type_name(f.kind).into()),
                "bypass": f.bypass, "preset": f.preset.and_then(|i| catalog::presets(f.kind).get(i as usize).map(|p| p.name)),
                "params": f.params, "error": st.fx_errors.get(&f.id),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "patterns": p.patterns.iter().map(|x| json!({"id": x.id, "name": x.name, "lenBeats": x.len, "notes": x.notes.len()})).collect::<Vec<_>>(),
        "sources": p.sources.iter().map(|s| json!({"id": s.id, "name": s.name, "seconds": s.dur, "path": s.path})).collect::<Vec<_>>(),
        "clips": p.clips.len(),
        "nextId": p.next_id,
    })
}

pub fn schema() -> Value {
    json!({
        "units": "time in beats (quarter notes); pattern note step/len in sixteenths; fades in seconds; vol = fader 0..1 (0.7937 = 0 dB); clip gain in dB; pan -1..1",
        "ids": "take new ids from context.nextId and count up",
        "ops": {
            "putClips": {"clips": [{"id": 0, "track": 0, "start": 0, "len": 4, "offset": 0, "kind": "pat|audio", "ref": "pattern or source id", "muted": false, "gain": 0, "fadeIn": 0, "fadeOut": 0, "pan": 0, "norm": false, "rev": false}]},
            "removeClips": {"ids": [0]},
            "putTrack": {"track": {"id": 0, "name": "", "color": "#4f8ef7", "muted": false, "solo": false, "arm": false, "vol": 0.7937, "h": 56, "insert": 1}},
            "putInsert": {"insert": {"id": 0, "name": "", "color": "#8b93a1", "vol": 0.7937, "pan": 0, "muted": false, "solo": false, "fx": []}},
            "removeInsert": {"id": 0},
            "putFx": {"insertId": 1, "fx": {"id": 0, "type": "comp", "bypass": false, "params": {"thr": -18}}, "index": null},
            "removeFx": {"fxId": 0},
            "moveFx": {"fxId": 0, "to": 0},
            "putPattern": {"pattern": {"id": 0, "name": "", "color": "#4f8ef7", "len": 4, "notes": [{"inst": "kick|clap|hat|bass", "step": 0, "len": 1, "p": 36}]}},
            "putSource": {"source": "rename/recolour an existing source"},
            "removeSource": {"id": 0},
            "setBpm": {"bpm": 128},
            "setLoop": {"loop": {"a": 0, "b": 16}},
        },
        "example": [{"op": "setBpm", "bpm": 140}, {"op": "removeClips", "ids": [3]}],
    })
}

pub fn import(app: &App, a: &Value) -> R<Value> {
    let paths: Vec<PathBuf> = arg(a, "paths").and_then(Value::as_array).ok_or("`paths` is required")?.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect();
    let place = match (opt_f64(a, "track"), opt_f64(a, "beat")) {
        (Some(t), b) => Some((t as u32, b.unwrap_or(0.0))),
        (None, Some(b)) => Some((0, b)),
        _ => None,
    };
    let (s, c) = app.engine.import_audio(&paths, place).map_err(|e| e.to_string())?;
    Ok(json!({ "sourceIds": s, "clipIds": c }))
}

/// The CLI entry: one request → JSON data.
pub fn control(app: &Arc<App>, cmd: &str, a: &Value, if_revision: Option<u64>) -> R<Value> {
    let e = &app.engine;
    let ok_rev = |r: u64| json!({ "revision": r });
    match cmd {
        "status" => {
            let st = e.state();
            Ok(json!({ "revision": st.revision, "transport": st.transport, "path": st.path, "dirty": st.dirty, "audio": app.audio_status(), "cli": *app.cli.lock() }))
        }
        "context" => Ok(context(app, arg(a, "full").and_then(Value::as_bool).unwrap_or(false))),
        "catalog" => Ok(json!({ "effects": catalog::catalog(), "commands": ["status","context","catalog","schema","apply","undo","redo","play","stop","render","import","add_fx","set_param","load_preset","save","open","vst3_scan","vst3_add","vst3_params","vst3_set_param"] })),
        "schema" => Ok(schema()),
        "apply" => {
            let ops: Vec<Op> = serde_json::from_value(arg(a, "ops").cloned().unwrap_or(json!([]))).map_err(|er| format!("BAD_OPS: {er}"))?;
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
        "set_param" => set_param(app, need_u64(a, "fxId")?, need_str(a, "param")?, opt_f64(a, "value").ok_or("`value` is required")?, if_revision).map(ok_rev),
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
        "vst3_scan" => Ok(json!(app.plugins.scan(arg(a, "rescan").and_then(Value::as_bool).unwrap_or(false)))),
        "vst3_add" => {
            let (id, rev) = vst3_add(app, need_u64(a, "insertId")?, need_str(a, "path")?, need_str(a, "cid")?, None)?;
            Ok(json!({ "fxId": id, "revision": rev }))
        }
        "vst3_params" => {
            let q = arg(a, "query").and_then(Value::as_str).map(str::to_lowercase);
            let ps = app.plugins.params(need_u64(a, "fxId")?)?;
            Ok(json!(ps.into_iter().filter(|p| q.as_ref().map(|q| p.name.to_lowercase().contains(q)).unwrap_or(true)).collect::<Vec<_>>()))
        }
        "vst3_set_param" => {
            app.plugins.set_param(need_u64(a, "fxId")?, need_u64(a, "id")? as u32, opt_f64(a, "value").ok_or("`value` is required")?)?;
            Ok(json!({}))
        }
        other => Err(format!("UNKNOWN_COMMAND: `{other}` (see `wave catalog`)")),
    }
}
