//! Stretching audio clips: pitch-preserving time-stretch (rendered offline, in the
//! background for playback) and resampling, through bounce, live playback, ops and files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wave_engine::*;

const SR: u32 = 48000;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wave-stretch-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A project with no effects anywhere, at 120 BPM (a beat is 0.5 s).
fn dry_project() -> Project {
    let mut p = Project::empty();
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    p.bpm = 120.0;
    p
}

/// A sine at `f1` for `t1` seconds, then `f2` until `secs`.
fn write_tones(path: &Path, f1: f64, t1: f64, f2: f64, secs: f64) {
    let mut ph = 0.0f64;
    let data: Vec<f32> = (0..(secs * SR as f64) as usize)
        .flat_map(|i| {
            let f = if (i as f64) < t1 * SR as f64 { f1 } else { f2 };
            ph += 2.0 * std::f64::consts::PI * f / SR as f64;
            let v = (0.5 * ph.sin()) as f32;
            [v, v]
        })
        .collect();
    wave_engine::sources::write_wav(path, &data, SR).unwrap();
}

/// An engine playing one audio clip of a two-tone file (440 Hz for 1 s, then 880 Hz for 1 s).
fn engine_with_clip(tag: &str) -> (Arc<Engine>, Clip) {
    let dir = tmp(tag);
    let path = dir.join("tones.wav");
    write_tones(&path, 440.0, 1.0, 880.0, 2.0);
    let e = Engine::new(dry_project(), Arc::new(NoPlugins));
    let (_, clips) = e.import_audio(&[path], Some((0, 0.0))).unwrap();
    let clip = e.project().clips.into_iter().find(|c| c.id == clips[0]).unwrap();
    assert!((clip.len - 4.0).abs() < 0.01, "2 s at 120 BPM is 4 beats, got {}", clip.len);
    (e, clip)
}

fn left(x: &[f32]) -> Vec<f32> {
    x.chunks(2).map(|f| f[0]).collect()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Frequency from upward zero crossings of `x[a..b]`.
fn freq(x: &[f32], a: usize, b: usize) -> f64 {
    let mut cross = vec![];
    for i in a.max(1)..b {
        if x[i - 1] < 0.0 && x[i] >= 0.0 {
            cross.push((i - 1) as f64 + (-x[i - 1] / (x[i] - x[i - 1])) as f64);
        }
    }
    assert!(cross.len() > 4, "no tone in {a}..{b}");
    (cross.len() - 1) as f64 / ((cross[cross.len() - 1] - cross[0]) / SR as f64)
}

fn sec(t: f64) -> usize {
    (t * SR as f64) as usize
}

fn put(e: &Engine, c: Clip) -> Result<u64, EngineError> {
    e.apply(&[Op::PutClips { clips: vec![c] }], false, None)
}

#[test]
fn time_stretch_keeps_pitch_and_doubles_the_length() {
    let (e, clip) = engine_with_clip("keep");
    put(&e, Clip { stretch: 2.0, len: 8.0, ..clip }).unwrap();
    let out = left(&e.render(0.0, 10.0, 0.0, SR).unwrap());
    // 0..2 s plays the 440 Hz half, 2..4 s the 880 Hz half, then silence
    let f1 = freq(&out, sec(0.4), sec(1.6));
    let f2 = freq(&out, sec(2.4), sec(3.6));
    assert!((f1 - 440.0).abs() < 5.0, "first half at {f1} Hz");
    assert!((f2 - 880.0).abs() < 9.0, "second half at {f2} Hz");
    assert!(rms(&out[sec(0.5)..sec(3.5)]) > 0.3, "level kept");
    assert!(rms(&out[sec(4.05)..sec(5.0)]) < 1e-3, "silent after the clip");
}

#[test]
fn resampling_changes_pitch_with_speed() {
    let (e, clip) = engine_with_clip("resample");
    put(&e, Clip { stretch: 2.0, len: 8.0, resample: true, ..clip }).unwrap();
    let out = left(&e.render(0.0, 10.0, 0.0, SR).unwrap());
    let f1 = freq(&out, sec(0.4), sec(1.6));
    let f2 = freq(&out, sec(2.4), sec(3.6));
    assert!((f1 - 220.0).abs() < 2.0, "half speed is an octave down: {f1} Hz");
    assert!((f2 - 440.0).abs() < 4.0, "half speed is an octave down: {f2} Hz");
}

#[test]
fn offset_counts_beats_of_the_stretched_audio() {
    // 4 beats into the clip stretched ×2 is 1 s into the file: the 880 Hz half
    for resample in [false, true] {
        let (e, clip) = engine_with_clip(if resample { "off-r" } else { "off-k" });
        put(&e, Clip { stretch: 2.0, offset: 4.0, len: 4.0, resample, ..clip }).unwrap();
        let out = left(&e.render(0.0, 4.0, 0.0, SR).unwrap());
        let f = freq(&out, sec(0.4), sec(1.6));
        let want = if resample { 440.0 } else { 880.0 };
        assert!((f - want).abs() < want * 0.012, "resample {resample}: {f} Hz, want {want}");
    }
}

#[test]
fn squeezing_plays_faster_at_the_same_pitch() {
    let (e, clip) = engine_with_clip("squeeze");
    put(&e, Clip { stretch: 0.5, len: 2.0, ..clip }).unwrap();
    let out = left(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    let f1 = freq(&out, sec(0.1), sec(0.4));
    let f2 = freq(&out, sec(0.6), sec(0.9));
    assert!((f1 - 440.0).abs() < 6.0 && (f2 - 880.0).abs() < 12.0, "{f1} / {f2} Hz");
    assert!(rms(&out[sec(1.05)..sec(1.9)]) < 1e-3, "the clip ends after 1 s");
}

#[test]
fn invalid_stretches_are_rejected() {
    let (e, clip) = engine_with_clip("invalid");
    for s in [0.0, 0.1, 5.0, f64::NAN, f64::INFINITY] {
        assert!(put(&e, Clip { stretch: s, ..clip.clone() }).is_err(), "stretch {s} accepted");
    }
    let mut p = e.project();
    let pat = Pattern { id: 77, name: "P".into(), color: "#fff".into(), len: 4.0, min_len: 4.0, notes: vec![], swing: 0.0 };
    p.patterns.push(pat.clone());
    e.apply(&[Op::PutPattern { pattern: pat }], false, None).unwrap();
    let pc = Clip { id: 78, kind: ClipKind::Pat, source: 77, stretch: 2.0, ..clip.clone() };
    assert!(put(&e, pc.clone()).is_err(), "a pattern clip cannot be stretched");
    assert!(put(&e, Clip { stretch: 1.0, resample: true, ..pc }).is_err());
    assert_eq!(e.project().clips.iter().find(|c| c.id == clip.id).unwrap().stretch, 1.0, "unchanged after a rejected op");
}

#[test]
fn stretch_is_saved_and_old_files_load_unstretched() {
    let (e, clip) = engine_with_clip("serde");
    let plain = serde_json::to_value(&clip).unwrap();
    assert!(plain.get("stretch").is_none() && plain.get("resample").is_none(), "unstretched clips are written as before");
    let back: Clip = serde_json::from_value(plain).unwrap();
    assert_eq!((back.stretch, back.resample), (1.0, false));
    let c = Clip { stretch: 1.5, len: 6.0, resample: true, ..clip };
    put(&e, c.clone()).unwrap();
    let path = tmp("serde").join("p.wave");
    e.save(&path).unwrap();
    let f = Engine::new(Project::empty(), Arc::new(NoPlugins));
    f.open(&path).unwrap();
    let got = f.project().clips.into_iter().find(|x| x.id == c.id).unwrap();
    assert_eq!((got.stretch, got.resample, got.len), (1.5, true, 6.0));
}

#[test]
fn live_playback_renders_the_stretch_in_the_background() {
    let (e, clip) = engine_with_clip("live");
    let mut rt = e.create_rt(SR as f32, 512);
    put(&e, Clip { stretch: 2.0, len: 8.0, ..clip.clone() }).unwrap();
    // queued at once; it plays resampled until the render lands
    let t = std::time::Instant::now();
    while e.state().stretching.contains(&clip.id) {
        assert!(t.elapsed().as_secs() < 20, "the stretch never finished");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(e.state().stretch_errors.is_empty());
    // then the live engine plays the time-stretch: the first 440 Hz half lasts 2 s
    e.play(0.0);
    let mut out = vec![];
    let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
    while out.len() < sec(1.6) {
        rt.process(&mut l, &mut r, None);
        out.extend_from_slice(&l);
    }
    e.stop();
    let f = freq(&out, sec(0.4), sec(1.5));
    assert!((f - 440.0).abs() < 5.0, "live playback at {f} Hz");
}

#[test]
fn undo_restores_the_unstretched_clip() {
    let (e, clip) = engine_with_clip("undo");
    put(&e, Clip { stretch: 2.0, len: 8.0, ..clip.clone() }).unwrap();
    e.undo();
    let got = e.project().clips.into_iter().find(|c| c.id == clip.id).unwrap();
    assert_eq!(got, clip);
    let out = left(&e.render(0.0, 12.0, 0.0, SR).unwrap());
    assert!(rms(&out[sec(2.05)..sec(2.9)]) < 1e-3, "plays 2 s again");
}

#[test]
fn pitch_transposes_and_keeps_the_length() {
    let (e, clip) = engine_with_clip("pitch");
    put(&e, Clip { pitch: 7.0, ..clip.clone() }).unwrap();
    let out = left(&e.render(0.0, 12.0, 0.0, SR).unwrap());
    let up = 2f64.powf(7.0 / 12.0);
    let (f1, f2) = (freq(&out, sec(0.2), sec(0.8)), freq(&out, sec(1.2), sec(1.8)));
    // within 10 cents
    let cents = |f: f64, want: f64| 1200.0 * (f / want).log2();
    assert!(cents(f1, 440.0 * up).abs() < 10.0, "first half at {f1} Hz");
    assert!(cents(f2, 880.0 * up).abs() < 10.0, "second half at {f2} Hz");
    // the length stays: sound until 2 s, then silence
    assert!(rms(&out[sec(1.5)..sec(1.9)]) > 0.3);
    assert!(rms(&out[sec(2.05)..sec(3.0)]) < 1e-3, "sound after the clip");

    // with a stretch: one render, both at once
    put(&e, Clip { pitch: -12.0, stretch: 2.0, len: 8.0, ..clip.clone() }).unwrap();
    let out = left(&e.render(0.0, 12.0, 0.0, SR).unwrap());
    let f1 = freq(&out, sec(0.4), sec(1.6));
    assert!(cents(f1, 220.0).abs() < 10.0, "stretched and an octave down: {f1} Hz");
    assert!(rms(&out[sec(3.5)..sec(3.9)]) > 0.3 && rms(&out[sec(4.05)..sec(5.0)]) < 1e-3, "4 s long");

    // resampled (turntable) and transposed: the speed sets the length, pitch moves on top
    put(&e, Clip { pitch: 12.0, stretch: 2.0, len: 8.0, resample: true, ..clip.clone() }).unwrap();
    let out = left(&e.render(0.0, 12.0, 0.0, SR).unwrap());
    let f1 = freq(&out, sec(0.4), sec(1.6));
    assert!(cents(f1, 440.0).abs() < 10.0, "half speed an octave up = 440 Hz: {f1} Hz");

    // back to 0: the source again, exactly
    put(&e, clip.clone()).unwrap();
    let out = left(&e.render(0.0, 3.0, 0.0, SR).unwrap());
    assert!((freq(&out, sec(0.2), sec(0.8)) - 440.0).abs() < 2.0);
}

#[test]
fn pitch_is_checked_and_saved() {
    let (e, clip) = engine_with_clip("pitchops");
    assert!(put(&e, Clip { pitch: 12.5, ..clip.clone() }).is_err());
    assert!(put(&e, Clip { pitch: f32::NAN, ..clip.clone() }).is_err());
    let pat = Clip { id: 77_000, kind: ClipKind::Pat, source: 1, start: 16.0, pitch: 2.0, ..clip.clone() };
    assert!(put(&e, pat).is_err(), "only audio clips are transposed");
    put(&e, Clip { pitch: -3.25, ..clip.clone() }).unwrap();
    let json = serde_json::to_value(e.project()).unwrap();
    let saved = json["clips"].as_array().unwrap().iter().find(|c| c["id"] == clip.id).unwrap();
    assert_eq!(saved["pitch"], -3.25);
    // 0 is left out, and a file without it loads as 0
    put(&e, clip.clone()).unwrap();
    let json = serde_json::to_value(e.project()).unwrap();
    let saved = json["clips"].as_array().unwrap().iter().find(|c| c["id"] == clip.id).unwrap();
    assert!(saved.get("pitch").is_none());
    let back: Clip = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(back.pitch, 0.0);
}
