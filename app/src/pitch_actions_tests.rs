//! The agent's journey through the same actions the app answers: a project file, a sung take
//! and a reference, inspect → compare → suggest → edit → render, then save, reopen and undo.

use std::path::{Path, PathBuf};
use std::sync::Once;

use serde_json::{json, Value};
use wave_cli::Files as _;
use wave_engine::singer::{sing, song, Voice};
use wave_engine::sources::{write_wav, Audio};

use crate::headless::Files;

const SR: f32 = 44100.0;

/// Pitch analysis in these tests is the built-in YIN: the same on every machine, whether or not
/// the RMVPE model is installed.
fn yin_only() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| std::env::set_var("WAVE_RMVPE_MODEL", std::env::temp_dir().join("wave-no-such-model.onnx")));
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wave-pitch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wav(path: &Path, mono: &[f32]) {
    let st: Vec<f32> = mono.iter().flat_map(|&x| [x, x]).collect();
    write_wav(path, &st, SR as u32).unwrap();
}

/// A project with a take (notes 1 and 4 sung 45 ct sharp / flat) and a reference singer an octave
/// up in another voice and tempo: (doc, take clip, reference clip, reference source).
fn project(d: &Path) -> (Box<dyn wave_cli::Doc>, u64, u64, u64) {
    yin_only();
    let s = song(41, 10);
    let mut detune = vec![0f32; s.len()];
    detune[1] = 45.0;
    detune[4] = -45.0;
    wav(&d.join("take.wav"), &sing(&s, &Voice::default(), &detune, 0.0, SR).audio);
    let rv = Voice { formant: 1.15, tempo: 0.93, jitter: 0.1, transpose: 12.0, vib_rate: 6.0, vib_depth: 0.25, seed: 7 };
    wav(&d.join("ref.wav"), &sing(&s, &rv, &[], 0.0, SR).audio);
    let doc = Files.create("empty").unwrap();
    let paths = [d.join("take.wav").display().to_string(), d.join("ref.wav").display().to_string()];
    let r = doc.call("import", &json!({ "paths": paths, "track": 0, "beat": 0 }), None).unwrap();
    let clip = r["clipIds"][0].as_u64().unwrap();
    let other = r["clipIds"][1].as_u64().unwrap();
    let src = r["sourceIds"][1].as_u64().unwrap();
    (doc, clip, other, src)
}

fn clip_json(doc: &dyn wave_cli::Doc, id: u64) -> Value {
    let ctx = doc.call("context", &json!({ "full": true }), None).unwrap();
    let clips = ctx["clips"].as_array().or_else(|| ctx["project"]["clips"].as_array()).expect("context --full lists clips");
    clips.iter().find(|c| c["id"] == id).cloned().unwrap()
}

fn rendered(doc: &dyn wave_cli::Doc, clip: u64, out: &Path, original: bool) -> Audio {
    doc.call("pitch_render", &json!({ "clipId": clip, "out": out.display().to_string(), "original": original }), None).unwrap();
    Audio::load(out).unwrap()
}

/// Numbers to 1e-9: a save and reload may differ from the live value in the last binary digit.
fn approx(v: &Value) -> Value {
    match v {
        Value::Number(n) => json!((n.as_f64().unwrap() * 1e9).round() / 1e9),
        Value::Array(a) => Value::Array(a.iter().map(approx).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), approx(x))).collect()),
        x => x.clone(),
    }
}

fn without_tune(mut c: Value) -> Value {
    c.as_object_mut().unwrap().remove("tune");
    c
}

#[test]
fn an_agent_corrects_slips_through_the_cli_actions_and_keeps_everything_else() {
    let d = tmp("journey");
    let (doc, clip, _, src) = project(&d);
    let doc = doc.as_ref();
    let ins = doc.call("pitch_inspect", &json!({ "clipId": clip }), None).unwrap();
    assert_eq!(ins["segmentation"], "analysis");
    let notes = ins["notes"].as_array().unwrap();
    assert!(notes.len() >= 8, "{} notes", notes.len());
    let base = ins["base"].as_str().unwrap().to_string();
    let full = doc.call("pitch_inspect", &json!({ "clipId": clip, "full": true, "from": 0.5, "to": 1.0 }), None).unwrap();
    assert_eq!(full["frames"]["f0"].as_array().unwrap().len(), 100, "5 ms frames over half a second");

    let cmp = doc.call("pitch_compare", &json!({ "clipId": clip, "source": src }), None).unwrap();
    assert_eq!(cmp["related"], true, "{cmp}");
    assert_eq!(cmp["intervals"][0]["semitones"], -12);
    assert_eq!(cmp["base"], base);

    let sug = doc.call("pitch_suggest", &json!({ "clipId": clip, "source": src }), None).unwrap();
    let edits: Vec<Value> = sug["suggestions"].as_array().unwrap().iter().filter(|s| s["decision"] == "edit").cloned().collect();
    assert!((2..=4).contains(&edits.len()), "{}", serde_json::to_string_pretty(&sug["suggestions"]).unwrap());
    assert!(edits.iter().all(|e| e["changeCents"].as_f64().unwrap().abs() > 25.0), "only the slips move: {edits:?}");
    let before = clip_json(doc, clip);
    let rev0 = doc.revision();
    // the suggestion's op, applied as the pitch editor sends its edits
    let r = doc.call("apply", &json!({ "ops": [sug["op"].clone()] }), Some(rev0)).unwrap();
    assert!(r["revision"].as_u64().unwrap() > rev0);
    assert_eq!(without_tune(clip_json(doc, clip)), without_tune(before), "nothing but the pitch edits changed");

    let orig = rendered(doc, clip, &d.join("orig.wav"), true);
    let out = rendered(doc, clip, &d.join("out.wav"), false);
    assert_eq!(orig.frames, out.frames);
    assert_eq!(orig.content(), Audio::load(&d.join("take.wav")).unwrap().content(), "--original is the recording itself");
    assert_ne!(orig.content(), out.content(), "the edits are audible");
    let re = doc.call("pitch_inspect", &json!({ "clipId": clip }), None).unwrap();
    assert_eq!(re["segmentation"], "edits");
    // the rendered audio, analysed again, sings the corrected notes in tune
    let ra = wave_engine::pitch::analyze(&wave_engine::sources::mono_of(&out), out.sr as f32);
    for e in &edits {
        let n = &re["notes"][e["i"].as_u64().unwrap() as usize];
        let (a, b) = (n["start"].as_f64().unwrap(), n["end"].as_f64().unwrap());
        let mut v: Vec<f32> =
            ra.curve[((a + 0.05) * 200.0) as usize..((b - 0.03) * 200.0) as usize].iter().copied().filter(|x| x.is_finite()).collect();
        v.sort_by(f32::total_cmp);
        let m = v[v.len() / 2];
        assert!((m - m.round()).abs() * 100.0 < 15.0, "rendered note {} at {m:.2}", e["i"]);
    }
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn edits_are_validated_atomic_and_guarded_against_stale_notes() {
    let d = tmp("guards");
    let (doc, clip, other, _) = project(&d);
    let doc = doc.as_ref();
    let ins = doc.call("pitch_inspect", &json!({ "clipId": clip }), None).unwrap();
    let base = ins["base"].as_str().unwrap().to_string();
    let rev = doc.revision();
    // a target in MIDI becomes the note's shift
    let target = ins["notes"][0]["sung"].as_f64().unwrap().round() + 1.0;
    let r = doc.call("pitch_edit", &json!({ "clipId": clip, "base": base, "notes": [{ "i": 0, "target": target }] }), Some(rev)).unwrap();
    assert_eq!(r["base"].as_str().unwrap(), base, "a move keeps the segmentation");
    let ins2 = doc.call("pitch_inspect", &json!({ "clipId": clip }), None).unwrap();
    assert!((ins2["notes"][0]["out"].as_f64().unwrap() - target).abs() < 0.05, "{}", ins2["notes"][0]);
    // the revision moved on: an edit planned on the old one is refused
    let e = doc.call("pitch_edit", &json!({ "clipId": clip, "notes": [{ "i": 1, "shift": 10 }] }), Some(rev)).unwrap_err();
    assert!(e.contains("REVISION_CONFLICT"), "{e}");
    // a split changes the segmentation: the old base is stale afterwards
    let n2 = &ins2["notes"][2];
    let at = (n2["start"].as_f64().unwrap() + n2["end"].as_f64().unwrap()) / 2.0;
    doc.call("pitch_edit", &json!({ "clipId": clip, "base": base, "split": [{ "i": 2, "at": at }] }), None).unwrap();
    let snapshot = clip_json(doc, clip);
    let e = doc.call("pitch_edit", &json!({ "clipId": clip, "base": base, "notes": [{ "i": 3, "shift": 10 }] }), None).unwrap_err();
    assert!(e.contains("STALE_NOTES"), "{e}");
    for (args, want) in [
        (json!({ "clipId": clip, "notes": [{ "i": 999, "shift": 10 }] }), "no note 999"),
        (json!({ "clipId": clip, "notes": [{ "i": 0, "shift": 10, "target": 60 }] }), "not both"),
        (json!({ "clipId": clip, "notes": [{ "i": 0, "shift": 300 }], "maxCents": 100 }), "OVER_LIMIT"),
        (json!({ "clipId": clip, "notes": [{ "i": 0, "drift": 3 }] }), "drift"),
        (json!({ "clipId": clip, "notez": [] }), "BAD_ARGS"),
        (json!({ "clipId": other + 1000, "notes": [{ "i": 0, "shift": 1 }] }), "clip"),
    ] {
        let e = doc.call("pitch_edit", &args, None).unwrap_err();
        assert!(e.contains(want), "{args}: {e}");
    }
    assert_eq!(clip_json(doc, clip), snapshot, "refused edits change nothing");
    // a batch is atomic: one bad op refuses the whole batch
    let e = doc.call(
        "apply",
        &json!({ "ops": [
            { "op": "editTune", "clip": clip, "edit": { "notes": [{ "i": 4, "shift": 20 }] } },
            { "op": "editTune", "clip": clip, "edit": { "notes": [{ "i": 77, "shift": 20 }] } }] }),
        None,
    );
    assert!(e.is_err());
    assert_eq!(clip_json(doc, clip), snapshot);
    assert!(clip_json(doc, other)["tune"].is_null(), "the reference clip is untouched");
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn edits_survive_save_and_reopen_and_render_the_same() {
    let d = tmp("reopen");
    let (doc, clip, _, _) = project(&d);
    let ins = doc.call("pitch_inspect", &json!({ "clipId": clip }), None).unwrap();
    let n5 = &ins["notes"][5];
    let at = (n5["start"].as_f64().unwrap() + n5["end"].as_f64().unwrap()) / 2.0;
    doc.call("pitch_edit", &json!({ "clipId": clip, "split": [{ "i": 5, "at": at }] }), None).unwrap();
    doc.call(
        "pitch_edit",
        &json!({ "clipId": clip, "notes": [{ "i": 1, "shift": -45, "drift": 0.3, "speed": 40 }, { "i": 3, "formant": -100, "gain": -2 }], "merge": [5] }),
        None,
    )
    .unwrap();
    let tune = clip_json(doc.as_ref(), clip)["tune"].clone();
    let a = rendered(doc.as_ref(), clip, &d.join("a.wav"), false);
    let path = d.join("song.wave");
    doc.save(&path).unwrap();
    let (doc2, w) = Files.open(&path).unwrap();
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(approx(&clip_json(doc2.as_ref(), clip)["tune"]), approx(&tune));
    let b = rendered(doc2.as_ref(), clip, &d.join("b.wav"), false);
    assert_eq!(a.content(), b.content(), "the same edits render the same audio after reopening");
    assert_ne!(a.content(), Audio::load(&d.join("take.wav")).unwrap().content(), "the edits are audible");
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn the_ui_op_and_the_cli_command_make_the_same_project() {
    let (d1, d2) = (tmp("same"), tmp("same2"));
    let (ui, clip, _, _) = project(&d1);
    let (cli, clip2, _, _) = project(&d2);
    assert_eq!(clip, clip2);
    let ins = cli.call("pitch_inspect", &json!({ "clipId": clip }), None).unwrap();
    let spans: Vec<[f64; 2]> =
        ins["notes"].as_array().unwrap().iter().map(|n| [n["start"].as_f64().unwrap(), n["end"].as_f64().unwrap()]).collect();
    let at = (spans[2][0] + spans[2][1]) / 2.0;
    // what the pitch editor sends: an editTune op with the analysis' notes as `init`
    let op = json!({ "op": "editTune", "clip": clip, "edit": { "init": spans, "notes": [{ "i": 0, "shift": 30 }], "split": [{ "i": 2, "at": at }] } });
    ui.call("apply", &json!({ "ops": [op] }), None).unwrap();
    cli.call("pitch_edit", &json!({ "clipId": clip, "notes": [{ "i": 0, "shift": 30 }], "split": [{ "i": 2, "at": at }] }), None).unwrap();
    assert_eq!(clip_json(ui.as_ref(), clip)["tune"], clip_json(cli.as_ref(), clip)["tune"]);
    ui.call("apply", &json!({ "ops": [{ "op": "editTune", "clip": clip, "edit": { "clear": true } }] }), None).unwrap();
    cli.call("pitch_edit", &json!({ "clipId": clip, "clear": true }), None).unwrap();
    let (u, c) = (clip_json(ui.as_ref(), clip), clip_json(cli.as_ref(), clip));
    assert_eq!(u["tune"], c["tune"]);
    assert!(u["tune"].is_null(), "clearing every edit plays the plain source again");
    std::fs::remove_dir_all(&d1).ok();
    std::fs::remove_dir_all(&d2).ok();
}

#[test]
fn an_old_project_with_pitch_edits_loads_and_edits_without_losing_fields() {
    let d = tmp("compat");
    let (doc, clip, _, src) = project(&d);
    // the shape projects had before editTune: notes without speed, pencil and a Tune to Reference
    let ins = doc.call("pitch_inspect", &json!({ "clipId": clip }), None).unwrap();
    let notes: Vec<Value> = ins["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| json!({ "start": n["start"], "end": n["end"], "shift": 0, "drift": 0, "formant": 0, "gain": 0 }))
        .collect();
    let mut c = clip_json(doc.as_ref(), clip);
    c["tune"] = json!({ "notes": notes, "pencil": [[0.31, 60.0], [0.32, 60.2]],
        "reference": { "source": src, "correction": 0.8, "vibrato": 1, "influence": 1, "octave": null, "key": 0, "scale": 0 } });
    doc.call("apply", &json!({ "ops": [{ "op": "putClips", "clips": [c] }] }), None).unwrap();
    let path = d.join("old.wave");
    doc.save(&path).unwrap();
    let (doc, _) = Files.open(&path).unwrap();
    let ins = doc.call("pitch_inspect", &json!({ "clipId": clip }), None).unwrap();
    assert_eq!(ins["segmentation"], "edits");
    assert_eq!(ins["reference"]["source"], src);
    assert_eq!(ins["pencil"]["points"], 2);
    doc.call("pitch_edit", &json!({ "clipId": clip, "base": ins["base"], "notes": [{ "i": 2, "shiftBy": 12 }] }), None).unwrap();
    let t = clip_json(doc.as_ref(), clip)["tune"].clone();
    assert!((t["reference"]["correction"].as_f64().unwrap() - 0.8).abs() < 1e-6, "Tune to Reference settings stay");
    assert_eq!(t["pencil"].as_array().unwrap().len(), 2, "pencil stays");
    assert_eq!(t["notes"][2]["shift"], 12.0);
    let r = doc.call("pitch_target", &json!({ "clipId": clip }), None).unwrap();
    assert!(r["score"].as_f64().unwrap() > 0.3, "Tune to Reference still aligns: {r}");
    let out = rendered(doc.as_ref(), clip, &d.join("tr.wav"), false);
    assert_ne!(out.content(), Audio::load(&d.join("take.wav")).unwrap().content());
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn an_unrelated_reference_suggests_nothing() {
    let d = tmp("unrelated");
    let (doc, clip, _, _) = project(&d);
    wav(&d.join("other.wav"), &sing(&song(999, 10), &Voice { seed: 3, formant: 1.1, ..Voice::default() }, &[], 0.0, SR).audio);
    let r = doc.call("import", &json!({ "paths": [d.join("other.wav").display().to_string()] }), None).unwrap();
    let other = r["sourceIds"][0].as_u64().unwrap();
    let sug = doc.call("pitch_suggest", &json!({ "clipId": clip, "source": other }), None).unwrap();
    assert_eq!(sug["reference"]["related"], false, "{}", sug["reference"]);
    assert_eq!(sug["summary"]["edit"], 0);
    assert!(sug["op"].is_null());
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn undo_and_redo_restore_the_tune() {
    let d = tmp("undo");
    let (doc, clip, _, _) = project(&d);
    let path = d.join("u.wave");
    doc.save(&path).unwrap();
    drop(doc);
    // the running app's engine, with its undo history
    let engine = wave_engine::Engine::new(wave_engine::Project::empty(), std::sync::Arc::new(wave_engine::NoPlugins));
    engine.open(&path).unwrap();
    let find = |e: &wave_engine::Engine| e.project().clips.iter().find(|c| c.id == clip).cloned().unwrap();
    let before = find(&engine);
    let op: wave_engine::Op = serde_json::from_value(
        json!({ "op": "editTune", "clip": clip, "edit": { "init": [[0.1, 0.4], [0.4, 0.9]], "notes": [{ "i": 1, "shift": -30 }] } }),
    )
    .unwrap();
    engine.apply(&[op], false, None).unwrap();
    assert!(find(&engine).tune.is_some());
    assert!(engine.undo());
    assert_eq!(find(&engine), before);
    assert!(engine.redo());
    assert_eq!(find(&engine).tune.as_ref().unwrap().notes[1].shift, -30.0);
    std::fs::remove_dir_all(&d).ok();
}
