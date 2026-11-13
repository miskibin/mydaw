//! The pitch editor's actions for scripts and agents (`wave pitch …`): inspect a vocal clip,
//! compare it with another performance, get a conservative correction proposal, edit notes
//! (the `editTune` op the editor sends too) and render the clip alone. The same functions
//! answer the running app and `wave --project FILE`.

use serde_json::{json, Value};
use wave_engine::pitch_api::{self, Decision, SuggestParams};
use wave_engine::tune_edit::{fingerprint, TuneEdit};
use wave_engine::Op;

use crate::actions::{arg, as_index, need_str, need_u64, opt_f64, Host, R};

pub const COMMANDS: &[&str] = &["pitch_inspect", "pitch_compare", "pitch_suggest", "pitch_edit", "pitch_render"];

pub fn control(app: &dyn Host, cmd: &str, a: &Value, if_revision: Option<u64>) -> Option<R<Value>> {
    Some(match cmd {
        "pitch_inspect" => inspect(app, a),
        "pitch_compare" => compare(app, a),
        "pitch_suggest" => suggest(app, a),
        "pitch_edit" => edit(app, a, if_revision),
        "pitch_render" => render(app, a),
        _ => return None,
    })
}

fn view(app: &dyn Host, a: &Value) -> R<wave_engine::engine::PitchView> {
    app.engine().pitch_view(need_u64(a, "clipId")?).map_err(|e| e.to_string())
}

fn round4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

/// `pitch_inspect {clipId, full?, from?, to?, all?}`.
pub fn inspect(app: &dyn Host, a: &Value) -> R<Value> {
    let e = app.engine();
    let v = view(app, a)?;
    let (r0, r1) = v.region();
    let all = arg(a, "all").and_then(Value::as_bool).unwrap_or(false);
    let stats = pitch_api::note_stats(&v.analysis, &v.notes, &v.out);
    let in_clip = |s: &pitch_api::NoteStats| s.end > r0 && s.start < r1;
    let shown: Vec<&pitch_api::NoteStats> = stats.iter().filter(|s| all || in_clip(s)).collect();
    let notes: Vec<Value> = shown
        .iter()
        .map(|s| {
            let mut j = json!(s);
            j["beats"] = json!([round4(v.beat_of(s.start)), round4(v.beat_of(s.end))]);
            if all {
                j["inClip"] = json!(in_clip(s));
            }
            j
        })
        .collect();
    let mut kinds = serde_json::Map::new();
    for s in &shown {
        let k = serde_json::to_value(s.kind).unwrap_or_default().as_str().unwrap_or("").to_string();
        *kinds.entry(k).or_insert(json!(0)) = json!(kinds.get(&k).and_then(Value::as_u64).unwrap_or(0) + 1);
    }
    let edited =
        shown.iter().filter(|s| s.shift != 0.0 || s.drift != 0.0 || s.formant != 0.0 || s.gain != 0.0 || s.speed.is_some()).count();
    let clipped: Vec<pitch_api::NoteStats> = stats.iter().filter(|s| in_clip(s)).cloned().collect();
    let (f0, f1) = ((r0 * v.analysis.rate as f64) as usize, ((r1 * v.analysis.rate as f64) as usize).min(v.analysis.curve.len()));
    let mut voiced: Vec<f32> = v.analysis.curve[f0.min(f1)..f1].iter().copied().filter(|x| x.is_finite()).collect();
    voiced.sort_by(f32::total_cmp);
    let q = |p: f32| voiced.get(((voiced.len().saturating_sub(1)) as f32 * p) as usize).copied();
    let range = match (q(0.05), q(0.95)) {
        (Some(lo), Some(hi)) => json!({ "low": lo, "high": hi, "lowName": pitch_api::note_name(lo), "highName": pitch_api::note_name(hi) }),
        _ => Value::Null,
    };
    let tune = v.clip.tune.clone().unwrap_or_default();
    let reference = tune.reference.as_ref().map(|r| {
        json!({
            "source": if r.melody.is_some() { Value::Null } else { json!(r.source) },
            "melodyChannel": r.melody.as_ref().map(|m| m.channel),
            "correction": r.correction, "vibrato": r.vibrato, "influence": r.influence, "octave": r.octave, "speed": r.speed,
            "score": v.target.as_ref().map(|t| t.score), "appliedOctave": v.target.as_ref().map(|t| t.octave),
            "refused": v.target.as_ref().and_then(|t| t.reason.clone()),
        })
    });
    let mut pencil_runs: Vec<[f32; 2]> = vec![];
    for p in &tune.pencil {
        match pencil_runs.last_mut() {
            Some(r) if p[0] - r[1] <= 0.03 => r[1] = p[0],
            _ => pencil_runs.push([p[0], p[0]]),
        }
    }
    let mut out = json!({
        "clipId": v.clip.id,
        "revision": e.revision(),
        "base": fingerprint(&v.spans()),
        "segmentation": if v.own { "edits" } else { "analysis" },
        "backend": v.analysis.backend,
        "rate": v.analysis.rate,
        "clip": {
            "source": v.clip.source, "track": v.clip.track, "start": v.clip.start, "len": v.clip.len, "offset": v.clip.offset,
            "stretch": v.clip.stretch, "pitch": v.clip.pitch, "norm": v.clip.norm, "rev": v.clip.rev, "bpm": v.bpm,
            "region": [round4(r0), round4(r1)], "sourceSeconds": round4(v.seconds), "sampleRate": v.sr,
        },
        "noteCount": stats.len(),
        "shown": shown.len(),
        "edited": edited,
        "kinds": kinds,
        "tuning": pitch_api::tuning(&clipped),
        "range": range,
        "voicedSeconds": round4(voiced.len() as f64 / v.analysis.rate as f64),
        "reference": reference,
        "pencil": { "points": tune.pencil.len(), "runs": pencil_runs },
        "notes": notes,
    });
    if arg(a, "full").and_then(Value::as_bool).unwrap_or(false) {
        let from = opt_f64(a, "from").unwrap_or(r0);
        let to = opt_f64(a, "to").unwrap_or(r1);
        if to.partial_cmp(&from) != Some(std::cmp::Ordering::Greater) {
            return Err("BAD_ARGS: `to` must be after `from` (seconds of the source)".into());
        }
        out["frames"] = json!(pitch_api::frames_of(&v.analysis, &v.out, from, to));
    }
    Ok(out)
}

/// The reference source: `source`, or `referenceClip` (an audio clip's source).
fn reference_source(app: &dyn Host, a: &Value, required: bool) -> R<Option<u64>> {
    if let Some(s) = arg(a, "source").and_then(as_index) {
        return Ok(Some(s));
    }
    if let Some(c) = arg(a, "referenceClip").and_then(as_index) {
        let p = app.engine().project();
        let clip = p.clips.iter().find(|x| x.id == c).ok_or(format!("no clip {c}"))?;
        if clip.kind != wave_engine::ClipKind::Audio {
            return Err(format!("clip {c} is not an audio clip"));
        }
        return Ok(Some(clip.source));
    }
    if required {
        return Err("`source` (the reference's source id) or `referenceClip` is required".into());
    }
    Ok(None)
}

/// `pitch_compare {clipId, source | referenceClip}`.
pub fn compare(app: &dyn Host, a: &Value) -> R<Value> {
    let e = app.engine();
    let cid = need_u64(a, "clipId")?;
    let src = reference_source(app, a, true)?.unwrap_or_default();
    let v = e.pitch_view(cid).map_err(|x| x.to_string())?;
    let c = e.pitch_compare(cid, src).map_err(|x| x.to_string())?;
    let mut j = json!(c);
    j["clipId"] = json!(cid);
    j["source"] = json!(src);
    j["revision"] = json!(e.revision());
    j["base"] = json!(fingerprint(&v.spans()));
    Ok(j)
}

fn key_arg(a: &Value, project: &wave_engine::Project) -> R<Option<(u32, u32)>> {
    let key = match arg(a, "key") {
        Some(Value::String(s)) => Some(
            pitch_api::NOTE_NAMES.iter().position(|n| n.eq_ignore_ascii_case(s)).ok_or(format!("BAD_ARGS: unknown key `{s}` (C … B)"))?
                as u32,
        ),
        Some(v) => Some(as_index(v).filter(|k| *k < 12).ok_or("BAD_ARGS: `key` is 0..11 or a note name")? as u32),
        None => None,
    };
    let scale = match arg(a, "scale") {
        Some(Value::String(s)) => Some(match s.to_ascii_lowercase().as_str() {
            "chromatic" => 0,
            "major" => 1,
            "minor" => 2,
            "major pentatonic" | "majorpentatonic" => 3,
            "minor pentatonic" | "minorpentatonic" => 4,
            _ => return Err(format!("BAD_ARGS: unknown scale `{s}` (chromatic, major, minor, major pentatonic, minor pentatonic)")),
        }),
        Some(v) => Some(as_index(v).filter(|k| *k <= 4).ok_or("BAD_ARGS: `scale` is 0..4 or a name")? as u32),
        None => None,
    };
    Ok(match (key, scale) {
        (Some(k), s) => Some((k, s.unwrap_or(1))),
        (None, Some(_)) => return Err("BAD_ARGS: `scale` needs `key`".into()),
        // the project's key (`wave key …`) when it has one
        (None, None) => project.key.map(|k| (k as u32, project.scale)),
    })
}

/// `pitch_suggest {clipId, source?|referenceClip?, minCents?, maxCents?, strength?, key?, scale?, tuning?}`.
pub fn suggest(app: &dyn Host, a: &Value) -> R<Value> {
    let e = app.engine();
    let cid = need_u64(a, "clipId")?;
    let v = e.pitch_view(cid).map_err(|x| x.to_string())?;
    let src = reference_source(app, a, false)?;
    let cmp = match src {
        Some(s) => Some(e.pitch_compare(cid, s).map_err(|x| x.to_string())?),
        None => None,
    };
    let num = |k: &str, lo: f64, hi: f64, d: f64| -> R<f64> {
        match opt_f64(a, k) {
            Some(x) if (lo..=hi).contains(&x) => Ok(x),
            Some(_) => Err(format!("BAD_ARGS: `{k}` must be {lo}..{hi}")),
            None => Ok(d),
        }
    };
    let p = SuggestParams {
        min_cents: num("minCents", 0.0, 100.0, 15.0)? as f32,
        max_cents: num("maxCents", 1.0, 1200.0, 60.0)? as f32,
        strength: (num("strength", 0.0, 100.0, 100.0)? / 100.0) as f32,
        key: key_arg(a, &e.project())?,
        tuning: match opt_f64(a, "tuning") {
            Some(t) if t.abs() <= 50.0 => Some(t as f32),
            Some(_) => return Err("BAD_ARGS: `tuning` is cents off A440, within ±50".into()),
            None => None,
        },
    };
    let (r0, r1) = v.region();
    let stats: Vec<pitch_api::NoteStats> =
        pitch_api::note_stats(&v.analysis, &v.notes, &v.out).into_iter().filter(|s| s.end > r0 && s.start < r1).collect();
    let centers = pitch_api::base_centers(&v.analysis, &v.notes, v.target.as_deref());
    let sug = pitch_api::suggest(&stats, &centers, cmp.as_ref(), &p);
    let edits: Vec<Value> = sug.iter().filter(|s| s.decision == Decision::Edit).map(|s| json!({ "i": s.i, "shift": s.shift })).collect();
    let mut edit = json!({ "base": fingerprint(&v.spans()), "notes": edits });
    if !v.own {
        edit["init"] = json!(v.notes.iter().map(|n| [n.start, n.end]).collect::<Vec<_>>());
    }
    let n_edit = sug.iter().filter(|s| s.decision == Decision::Edit).count();
    Ok(json!({
        "clipId": cid,
        "revision": e.revision(),
        "base": fingerprint(&v.spans()),
        "params": { "minCents": p.min_cents, "maxCents": p.max_cents, "strength": p.strength, "key": p.key.map(|k| k.0), "scale": p.key.map(|k| k.1), "tuning": p.tuning },
        "reference": cmp.as_ref().map(|c| json!({
            "source": src, "related": c.related, "score": c.score, "reason": c.reason,
            "interval": c.intervals.first(), "ambiguousInterval": c.ambiguous_interval, "coverage": c.coverage,
            "sameNotes": c.same_notes, "referenceTuningCents": c.reference_tuning_cents,
        })),
        "summary": { "notes": sug.len(), "edit": n_edit, "keep": sug.len() - n_edit },
        "suggestions": sug,
        "op": if n_edit > 0 { json!({ "op": "editTune", "clip": cid, "edit": edit }) } else { Value::Null },
    }))
}

/// `pitch_edit {clipId, base?, notes?: [{i, shift?|target?|shiftBy?, drift?, formant?, gain?, speed?}], reset?, split?,
/// merge?, clear?, pencil?, erasePencil?, maxCents?}`: one `editTune` op (one undo step). `target` is
/// a MIDI pitch for the note's centre (turned into its `shift` here); the analysis' notes are
/// taken as the start when the clip has none of its own.
pub fn edit(app: &dyn Host, a: &Value, if_revision: Option<u64>) -> R<Value> {
    let e = app.engine();
    let cid = need_u64(a, "clipId")?;
    let v = e.pitch_view(cid).map_err(|x| x.to_string())?;
    let mut body = a.clone();
    let obj = body.as_object_mut().ok_or("BAD_ARGS: arguments must be an object")?;
    obj.remove("clipId");
    let max_cents = obj.remove("maxCents").and_then(|x| x.as_f64());
    let mut centers: Option<Vec<Option<f32>>> = None;
    if let Some(Value::Array(ns)) = obj.get_mut("notes") {
        for n in ns.iter_mut() {
            let Some(m) = n.as_object_mut() else { return Err("BAD_ARGS: each of `notes` is an object {i, …}".into()) };
            if let Some(t) = m.remove("target") {
                let t = t.as_f64().filter(|t| (0.0..=127.0).contains(t)).ok_or("BAD_ARGS: `target` is a MIDI pitch 0..127")? as f32;
                if m.contains_key("shift") || m.contains_key("shiftBy") {
                    return Err("BAD_ARGS: give a note `target` or `shift`/`shiftBy`, not both".into());
                }
                let i = m.get("i").and_then(as_index).ok_or("BAD_ARGS: each note needs `i`")? as usize;
                let cs = centers.get_or_insert_with(|| pitch_api::base_centers(&v.analysis, &v.notes, v.target.as_deref()));
                let c = cs.get(i).copied().ok_or(format!("notes: no note {i} (the clip has {})", v.notes.len()))?;
                let c = c.ok_or(format!("notes: note {i} has no voiced pitch to move to a target"))?;
                m.insert("shift".into(), json!(((t - c) * 100.0).round()));
            }
        }
    }
    let touches =
        ["notes", "reset", "split", "merge"].iter().any(|k| obj.get(*k).is_some_and(|x| x.as_array().is_some_and(|a| !a.is_empty())));
    if !v.own && touches && !obj.contains_key("init") {
        obj.insert("init".into(), json!(v.notes.iter().map(|n| [n.start, n.end]).collect::<Vec<_>>()));
    }
    let te: TuneEdit = serde_json::from_value(body.clone()).map_err(|er| format!("BAD_ARGS: {er}"))?;
    if let Some(lim) = max_cents {
        // what the notes' shifts will be once applied: refuse rather than move further than asked
        let after = wave_engine::tune_edit::apply(v.clip.tune.as_ref(), &te).map_err(|m| format!("BAD_OPS: {m}"))?;
        if let Some((i, n)) = after.iter().flat_map(|t| t.notes.iter().enumerate()).find(|(_, n)| (n.shift.abs() as f64) > lim) {
            return Err(format!("OVER_LIMIT: note {i} would be shifted {:+.0} ct, beyond maxCents {lim}", n.shift));
        }
    }
    let rev = e.apply(&[Op::EditTune { clip: cid, edit: te }], false, if_revision).map_err(|x| x.to_string())?;
    let p = e.project();
    let clip = p.clips.iter().find(|c| c.id == cid).ok_or("the clip is gone")?;
    let notes: Vec<(f64, f64)> = clip.tune.as_ref().map(|t| t.notes.iter().map(|n| (n.start, n.end)).collect()).unwrap_or_default();
    let base = if notes.is_empty() {
        fingerprint(&v.analysis.notes.iter().map(|n| (n.start, n.end)).collect::<Vec<_>>())
    } else {
        fingerprint(&notes)
    };
    Ok(json!({ "revision": rev, "clipId": cid, "base": base, "notes": notes.len(), "edited": clip.tune.is_some() }))
}

/// `pitch_render {clipId, out, original?}`: the clip's region of its source with its pitch edits
/// (before time-stretch, gain and fades), as a 32-bit float WAV; `original` = without the edits.
pub fn render(app: &dyn Host, a: &Value) -> R<Value> {
    let e = app.engine();
    let cid = need_u64(a, "clipId")?;
    let out = std::path::PathBuf::from(need_str(a, "out")?);
    let original = arg(a, "original").and_then(Value::as_bool).unwrap_or(false);
    let t0 = std::time::Instant::now();
    let (audio, (r0, r1)) = e.clip_tuned_audio(cid, !original).map_err(|x| x.to_string())?;
    let sr = audio.sr;
    let (i0, i1) = (((r0.max(0.0) * sr as f64) as usize).min(audio.frames), ((r1 * sr as f64) as usize).min(audio.frames));
    let x = &audio.content()[i0 * 2..i1.max(i0) * 2];
    wave_engine::sources::write_wav(&out, x, sr).map_err(|er| format!("{}: {er}", out.display()))?;
    Ok(json!({
        "out": out.display().to_string(), "clipId": cid, "original": original, "region": [round4(r0), round4(r1)],
        "stats": crate::actions::stats(x, sr), "renderSeconds": round4(t0.elapsed().as_secs_f64()),
    }))
}

#[cfg(test)]
#[path = "pitch_actions_tests.rs"]
mod tests;
