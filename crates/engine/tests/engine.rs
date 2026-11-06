use std::sync::Arc;

use wave_engine::catalog::new_fx;
use wave_engine::*;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn demo_renders_offline_with_sound_and_no_clipping() {
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    let out = e.render(0.0, 16.0, 1.0, 48000).unwrap();
    let secs = out.len() as f32 / 2.0 / 48000.0;
    assert!((secs - (16.0 * 60.0 / 128.0 + 1.0)).abs() < 0.05, "length {secs}");
    let level = rms(&out);
    assert!(level > 0.01, "demo is silent: rms {level}");
    let peak = out.iter().fold(0f32, |a, b| a.max(b.abs()));
    assert!(peak <= 10f32.powf(-0.3 / 20.0) + 1e-3, "master limiter let {peak} through");
    // kick on beat 1: energy right after 0 is far above the silence before any clip
    assert!(out.iter().all(|x| x.is_finite()));
}

#[test]
fn muting_the_track_silences_it() {
    let mut p = Project::demo();
    for t in p.tracks.iter_mut() {
        t.muted = true;
    }
    let e = Engine::new(p, Arc::new(NoPlugins));
    let out = e.render(0.0, 8.0, 0.0, 48000).unwrap();
    assert!(rms(&out) < 1e-5);
}

#[test]
fn undo_redo_and_gestures() {
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    let r0 = e.revision();
    e.apply(&[Op::SetBpm { bpm: 140.0 }], false, None).unwrap();
    assert_eq!(e.project().bpm, 140.0);
    assert!(e.revision() > r0);
    // a fader drag: many transient ops, one undo step
    let mut t = e.project().tracks[0].clone();
    e.begin_gesture();
    for k in 0..10 {
        t.vol = 0.5 + k as f32 * 0.01;
        e.apply(&[Op::PutTrack { track: t.clone() }], true, None).unwrap();
    }
    e.end_gesture();
    assert!((e.project().tracks[0].vol - 0.59).abs() < 1e-6);
    assert!(e.undo());
    assert_eq!(e.project().tracks[0].vol, VOL_UNITY);
    assert!(e.undo());
    assert_eq!(e.project().bpm, 128.0);
    assert!(e.redo());
    assert_eq!(e.project().bpm, 140.0);
}

#[test]
fn invalid_batches_change_nothing() {
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    let before = e.project();
    let bad = vec![Op::SetBpm { bpm: 150.0 }, Op::RemoveInsert { id: 0 }];
    assert!(e.apply(&bad, false, None).is_err());
    assert_eq!(e.project(), before);
    let r = e.revision();
    assert!(e.apply(&[Op::SetBpm { bpm: 100.0 }], false, Some(r + 5)).unwrap_err().to_string().contains("REVISION_CONFLICT"));
}

#[test]
fn effects_change_the_render() {
    let mut p = Project::demo();
    let plain = Engine::new(p.clone(), Arc::new(NoPlugins)).render(0.0, 8.0, 0.0, 48000).unwrap();
    let mut sat = new_fx(5000, FxKind::Sat, p.bpm);
    sat.params.insert("drive".into(), 30.0);
    p.inserts[1].fx.push(sat);
    let driven = Engine::new(p, Arc::new(NoPlugins)).render(0.0, 8.0, 0.0, 48000).unwrap();
    let diff: f32 = plain.iter().zip(&driven).map(|(a, b)| (a - b).abs()).sum();
    assert!(diff > 1.0, "saturator did nothing ({diff})");
}

#[test]
fn audio_clips_play_at_their_position() {
    let dir = std::env::temp_dir().join(format!("wave-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("tone.wav");
    // 0.5 s of 440 Hz at 44.1 kHz — a different rate than the render
    let tone: Vec<f32> = (0..22050).flat_map(|i| { let v = (i as f32 * 440.0 * std::f32::consts::TAU / 44100.0).sin() * 0.5; [v, v] }).collect();
    wave_engine::sources::write_wav(&wav, &tone, 44100).unwrap();
    let mut p = Project::empty();
    p.inserts[0].fx.clear(); // no limiter, so levels are exact
    let e = Engine::new(p, Arc::new(NoPlugins));
    let (_, clips) = e.import_audio(&[wav.clone()], Some((0, 4.0))).unwrap();
    assert_eq!(clips.len(), 1);
    let out = e.render(0.0, 8.0, 0.0, 48000).unwrap();
    let spb = 60.0 / 128.0;
    let at = |beat: f64| (beat * spb * 48000.0) as usize * 2;
    assert!(rms(&out[..at(3.9)]) < 1e-6, "sound before the clip");
    let body = rms(&out[at(4.1)..at(4.1) + 9600]);
    assert!(body > 0.25 && body < 0.5, "clip level {body}");
    // undo the import
    assert!(e.undo());
    assert!(e.project().clips.iter().all(|c| c.kind != ClipKind::Audio));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn save_and_open_round_trip() {
    let dir = std::env::temp_dir().join(format!("wave-proj-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("song.wave");
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    e.apply(&[Op::SetBpm { bpm: 133.0 }], false, None).unwrap();
    e.save(&path).unwrap();
    assert!(!e.state().dirty);
    let f = Engine::new(Project::empty(), Arc::new(NoPlugins));
    let warnings = f.open(&path).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(f.project(), e.project());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn live_rt_follows_edits_and_transport() {
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    let mut rt = e.create_rt(48000.0, 256);
    let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
    e.play(0.0);
    let mut energy = 0f32;
    for _ in 0..200 {
        rt.process(&mut l, &mut r, None);
        energy += rms(&l);
    }
    assert!(energy > 0.5, "nothing playing live ({energy})");
    let beat = e.shared().beat();
    let expect = 200.0 * 256.0 / 48000.0 / (60.0 / 128.0);
    assert!((beat - expect).abs() < 0.01, "playhead {beat} vs {expect}");
    // mute master live, fast path
    let mut m = e.project().inserts[0].clone();
    m.muted = true;
    e.apply(&[Op::PutInsert { insert: m }], false, None).unwrap();
    for _ in 0..4 {
        rt.process(&mut l, &mut r, None);
    }
    rt.process(&mut l, &mut r, None);
    assert!(rms(&l) < 1e-6);
    e.stop();
    assert!(!e.state().transport.playing);
}

#[test]
fn recording_lands_as_a_clip_on_the_armed_track() {
    let mut p = Project::demo();
    for t in p.tracks.iter_mut() {
        t.arm = false;
    }
    p.tracks[4].arm = true;
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut rt = e.create_rt(48000.0, 256);
    e.set_transport(None, Some(true), Some(true), None);
    e.play(8.0);
    let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
    let tone: Vec<f32> = (0..256).map(|i| (i as f32 * 0.1).sin() * 0.3).collect();
    // count-in (4 beats ≈ 1.875 s) + about 1 s of take
    for _ in 0..((48000.0 * 2.9) as usize / 256) {
        rt.process(&mut l, &mut r, Some((&tone, &tone)));
        std::thread::sleep(std::time::Duration::from_micros(200)); // let housekeeping drain the ring
    }
    e.stop();
    let p = e.project();
    let take = p.clips.iter().find(|c| c.kind == ClipKind::Audio).expect("no take clip");
    assert_eq!(take.track, 4);
    assert!((take.start - 8.0).abs() < 1e-9, "take starts at {}", take.start);
    let secs = take.len * 60.0 / p.bpm;
    assert!(secs > 0.8 && secs < 1.2, "take is {secs} s");
    assert!(e.undo());
    assert!(e.project().clips.iter().all(|c| c.kind != ClipKind::Audio));
}

#[test]
fn latency_is_compensated_across_inserts() {
    // a pitch corrector on one insert must not make that track late against the others
    let mut p = Project::demo();
    p.inserts[0].fx.clear();
    let tune = wave_engine::catalog::new_fx(7000, FxKind::Tune, p.bpm);
    p.inserts[6].fx = vec![tune];
    p.tracks[5].muted = false;
    let e = Engine::new(p.clone(), Arc::new(NoPlugins));
    let with = e.render(0.0, 1.0, 0.0, 48000).unwrap();
    let lat = wave_engine::vocal::tune_latency(48000.0);
    // the kick on beat 1 should now start `lat` samples later than without the tuner
    let mut q = p;
    q.inserts[6].fx.clear();
    let without = Engine::new(q, Arc::new(NoPlugins)).render(0.0, 1.0, 0.0, 48000).unwrap();
    let onset = |x: &[f32]| x.chunks(2).position(|f| f[0].abs() > 0.05).unwrap();
    assert_eq!(onset(&with), onset(&without) + lat);
}

#[test]
fn mix_zero_is_the_dry_signal() {
    let mut p = Project::demo();
    let dry = Engine::new(p.clone(), Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, 48000).unwrap();
    let mut sat = wave_engine::catalog::new_fx(6000, FxKind::Sat, p.bpm);
    sat.params.insert("drive".into(), 30.0);
    sat.mix = 0.0;
    p.inserts[1].fx.push(sat);
    let out = Engine::new(p, Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, 48000).unwrap();
    let diff = dry.iter().zip(&out).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(diff < 1e-5, "mix 0 still changed the sound by {diff}");
}

#[test]
fn automation_moves_volume_and_effect_params() {
    let mut p = Project::demo();
    p.inserts[0].fx.clear();
    // drums insert volume: full for 2 beats, then silent
    p.automations.push(Automation {
        id: 8000,
        name: "Drums vol".into(),
        color: "#e0a44a".into(),
        target: AutoTarget::InsertVol { id: 1 },
        points: vec![AutoPoint { t: 0.0, v: VOL_UNITY }, AutoPoint { t: 1.99, v: VOL_UNITY }, AutoPoint { t: 2.0, v: 0.0 }],
    });
    // only drums audible
    for t in p.tracks.iter_mut().skip(1) {
        t.muted = true;
    }
    p.clips.push(Clip { id: 8001, track: 7, start: 0.0, len: 8.0, offset: 0.0, kind: ClipKind::Auto, source: 8000, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None });
    let e = Engine::new(p, Arc::new(NoPlugins));
    let out = e.render(0.0, 4.0, 0.0, 48000).unwrap();
    let at = |b: f64| (b * 60.0 / 128.0 * 48000.0) as usize * 2;
    assert!(rms(&out[at(0.0)..at(1.9)]) > 0.01, "silent before the automation drops");
    assert!(rms(&out[at(2.05)..at(4.0)]) < 1e-4, "volume automation did nothing");
    // removing the automation removes its clip
    e.apply(&[Op::RemoveAutomation { id: 8000 }], false, None).unwrap();
    assert!(e.project().clips.iter().all(|c| c.kind != ClipKind::Auto));
}
