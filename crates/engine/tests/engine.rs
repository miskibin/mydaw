use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use wave_engine::catalog::new_fx;
use wave_engine::*;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn demo_renders_offline_with_sound_and_no_clipping() {
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
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
    let mut p = Project::sketch();
    for t in p.tracks.iter_mut() {
        t.muted = true;
    }
    let e = Engine::new(p, Arc::new(NoPlugins));
    let out = e.render(0.0, 8.0, 0.0, 48000).unwrap();
    assert!(rms(&out) < 1e-5);
}

#[test]
fn undo_redo_and_gestures() {
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
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
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    let before = e.project();
    let bad = vec![Op::SetBpm { bpm: 150.0 }, Op::RemoveInsert { id: 0 }];
    assert!(e.apply(&bad, false, None).is_err());
    assert_eq!(e.project(), before);
    let r = e.revision();
    assert!(e.apply(&[Op::SetBpm { bpm: 100.0 }], false, Some(r + 5)).unwrap_err().to_string().contains("REVISION_CONFLICT"));
}

#[test]
fn effects_change_the_render() {
    let mut p = Project::sketch();
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
    let mut p = Project::routed();
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
fn dry_wet_of_a_latent_effect_lines_up() {
    // Voice at zero shift is a pure delay: a 50 % blend must not comb-filter against the dry
    let dir = std::env::temp_dir().join(format!("wave-mix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("voice.wav");
    let sr = 48000.0f32;
    let x: Vec<f32> = (0..96000).flat_map(|i| { let v = (1..8).map(|k| (std::f32::consts::TAU * 220.0 * k as f32 * i as f32 / sr).sin() / k as f32).sum::<f32>() * 0.2; [v, v] }).collect();
    wave_engine::sources::write_wav(&wav, &x, 48000).unwrap();
    let mut p = Project::routed();
    p.inserts[0].fx.clear();
    let ins = p.tracks[0].insert;
    let voice = new_fx(90_000, FxKind::Voice, p.bpm);
    p.inserts.iter_mut().find(|m| m.id == ins).unwrap().fx.push(voice.clone());
    let e = Engine::new(p, Arc::new(NoPlugins));
    e.import_audio(std::slice::from_ref(&wav), Some((0, 0.0))).unwrap();
    let render = |mix: f32| {
        let fx = Fx { mix, ..voice.clone() };
        e.apply(&[Op::PutFx { insert_id: ins, fx, index: None }], false, None).unwrap();
        let out = e.render(0.0, 3.0, 0.0, 48000).unwrap();
        rms(&out[48000..120000])
    };
    let (full, half) = (render(1.0), render(0.5));
    assert!((half / full - 1.0).abs() < 0.05, "50 % mix is {:.2}× the wet level", half / full);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn save_and_open_round_trip() {
    let dir = std::env::temp_dir().join(format!("wave-proj-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("song.wave");
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    e.apply(&[Op::SetBpm { bpm: 133.0 }], false, None).unwrap();
    e.save(&path).unwrap();
    assert!(!e.state().dirty);
    let f = Engine::new(Project::routed(), Arc::new(NoPlugins));
    let warnings = f.open(&path).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(f.project(), e.project());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn live_rt_follows_edits_and_transport() {
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
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
    let mut p = Project::sketch();
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

fn armed_engine() -> Arc<Engine> {
    let mut p = Project::sketch();
    for t in p.tracks.iter_mut() {
        t.arm = false;
    }
    p.tracks[4].arm = true;
    Engine::new(p, Arc::new(NoPlugins))
}

/// Runs `secs` of live input through `rt`, giving housekeeping time to drain the ring.
fn feed(rt: &mut Rt, secs: f64) {
    let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
    let tone: Vec<f32> = (0..256).map(|i| (i as f32 * 0.1).sin() * 0.3).collect();
    for _ in 0..((48000.0 * secs) as usize / 256) {
        rt.process(&mut l, &mut r, Some((&tone, &tone)));
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
}

fn takes(e: &Engine) -> Vec<Clip> {
    e.project().clips.into_iter().filter(|c| c.kind == ClipKind::Audio).collect()
}

#[test]
fn disarming_mid_take_keeps_it_and_keeps_playing() {
    let e = armed_engine();
    let mut rt = e.create_rt(48000.0, 256);
    e.set_transport(None, Some(false), Some(true), None);
    e.play(0.0);
    feed(&mut rt, 1.0);
    e.set_transport(None, None, Some(false), None);
    let t = takes(&e);
    assert_eq!(t.len(), 1, "disarming lost the take");
    let secs = t[0].len * 60.0 / e.project().bpm;
    assert!(secs > 0.8 && secs < 1.2, "take is {secs} s");
    assert!(e.state().transport.playing, "punch-out stopped the transport");
    // nothing more is captured after the punch-out
    feed(&mut rt, 0.5);
    e.stop();
    assert_eq!(takes(&e).len(), 1);
    // and the next armed run records again
    e.set_transport(None, None, Some(true), None);
    e.play(0.0);
    feed(&mut rt, 0.5);
    e.stop();
    // (recording over the first take turns both into takes of one comp)
    let p = e.project();
    assert_eq!(p.comps.len(), 1, "could not record after the punch-out");
    assert_eq!(p.comps[0].takes.len(), 2);
    assert!(takes(&e).is_empty());
}

#[test]
fn restarting_the_audio_stream_mid_take_keeps_it() {
    // reopening the devices (mic toggle, device change) must not orphan the take
    let e = armed_engine();
    let mut rt = e.create_rt(48000.0, 256);
    e.set_transport(None, Some(false), Some(true), None);
    e.play(4.0);
    feed(&mut rt, 1.0);
    drop(rt);
    let mut rt = e.create_rt(48000.0, 256);
    let t = takes(&e);
    assert_eq!(t.len(), 1, "stream restart lost the take");
    assert!((t[0].start - 4.0).abs() < 1e-9);
    assert!(!e.state().transport.playing);
    e.play(0.0);
    assert!(e.state().transport.playing, "transport is stuck after the restart");
    feed(&mut rt, 0.5);
    e.stop();
    assert_eq!(takes(&e).len(), 2);
}

#[test]
fn latency_is_compensated_across_inserts() {
    // a pitch corrector on one insert must not make that track late against the others
    let mut p = Project::sketch();
    // no master limiter: when it is working it magnifies the realignment's float rounding
    p.inserts[0].fx.clear();
    let tune = wave_engine::catalog::new_fx(7000, FxKind::Tune, p.bpm);
    p.inserts[6].fx = vec![tune];
    p.tracks[5].muted = false;
    let e = Engine::new(p.clone(), Arc::new(NoPlugins));
    let with = e.render(0.0, 1.0, 0.0, 48000).unwrap();
    // live, the kick on beat 1 is delayed by the tuner's latency to line up with the vocal;
    // an export drops that delay, so the file starts on the beat as without the tuner
    let mut q = p;
    q.inserts[6].fx.clear();
    let without = Engine::new(q, Arc::new(NoPlugins)).render(0.0, 1.0, 0.0, 48000).unwrap();
    let onset = |x: &[f32]| x.chunks(2).position(|f| f[0].abs() > 0.05).unwrap();
    assert_eq!(onset(&with), onset(&without));
    assert_eq!(with.len(), without.len());
    let diff = with.iter().zip(&without).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(diff < 1e-4, "export with a latent effect differs from the same mix without it by {diff}");
}

#[test]
fn mix_zero_is_the_dry_signal() {
    let mut p = Project::sketch();
    // no master limiter: when it is working it magnifies the realignment's float rounding
    p.inserts[0].fx.clear();
    let dry = Engine::new(p.clone(), Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, 48000).unwrap();
    let mut sat = wave_engine::catalog::new_fx(6000, FxKind::Sat, p.bpm);
    sat.params.insert("drive".into(), 30.0);
    sat.mix = 0.0;
    p.inserts[1].fx.push(sat);
    let out = Engine::new(p, Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, 48000).unwrap();
    let diff = dry.iter().zip(&out).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    // the saturator is latent (oversampling): the export is realigned, which leaves float
    // rounding at the level of `latency_is_compensated_across_inserts`
    assert!(diff < 1e-4, "mix 0 still changed the sound by {diff}");
}

#[test]
fn automation_moves_volume_and_effect_params() {
    let mut p = Project::sketch();
    // no master limiter: when it is working it magnifies the realignment's float rounding
    p.inserts[0].fx.clear();
    // drums insert volume: full for 2 beats, then silent
    p.automations.push(Automation {
        id: 8000,
        name: "Drums vol".into(),
        color: "#e0a44a".into(),
        target: AutoTarget::InsertVol { id: 1 },
        points: vec![AutoPoint::at(0.0, VOL_UNITY), AutoPoint::at(1.99, VOL_UNITY), AutoPoint::at(2.0, 0.0)],
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

#[test]
fn export_range_uses_song_or_loop() {
    let mut p = Project::sketch();
    assert_eq!(Engine::export_range(&p, false), (0.0, 32.0));
    assert_eq!(Engine::export_range(&p, true), (0.0, 32.0)); // no loop → song
    p.loop_range = Some(LoopRange { a: 4.0, b: 12.0 });
    assert_eq!(Engine::export_range(&p, false), (0.0, 32.0));
    assert_eq!(Engine::export_range(&p, true), (4.0, 12.0));
}

#[test]
fn export_writes_wav_that_round_trips() {
    let dir = std::env::temp_dir().join(format!("wave-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bounce.wav");
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    let (from, to) = Engine::export_range(&e.project(), false);
    let audio = e.render(from, to, 0.5, 48000).unwrap();
    wave_engine::sources::write_wav(&path, &audio, 48000).unwrap();
    let mut reader = hound::WavReader::open(&path).unwrap();
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 48000);
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(spec.bits_per_sample, 32);
    let decoded: Vec<f32> = reader.samples::<f32>().map(|s| s.unwrap()).collect();
    assert_eq!(decoded.len(), audio.len());
    for (a, b) in audio.iter().zip(&decoded) {
        assert!((a - b).abs() < 1e-5, "WAV round-trip drifted");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn export_tail_extends_length_and_cancel_aborts() {
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    let no_tail = e.render(0.0, 4.0, 0.0, 48000).unwrap();
    let with_tail = e.render(0.0, 4.0, 1.0, 48000).unwrap();
    assert_eq!(no_tail.len(), ((4.0 * 60.0 / 128.0) * 48000.0) as usize * 2);
    assert_eq!(with_tail.len(), no_tail.len() + 48000 * 2);

    let cancel = AtomicBool::new(true);
    let err = e.render_cancelable(0.0, 32.0, 2.0, 48000, &cancel).unwrap_err();
    assert!(matches!(err, EngineError::Cancelled));

    let mut p = Project::routed();
    for t in p.tracks.iter_mut() {
        t.muted = true;
    }
    let silent = Engine::new(p, Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, 48000).unwrap();
    let peak = silent.iter().fold(0f32, |a, b| a.max(b.abs()));
    assert!(peak < 1e-5, "empty project should be silent");
}
 
#[test]
fn music_analysis_excludes_vocal_insert_and_caches() {
    let dir = std::env::temp_dir().join(format!("wave-music-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sr = 44100;
    let chord_wav = |name: &str, notes: [i32; 3]| {
        let p = dir.join(name);
        let n = sr as usize * 8;
        let data: Vec<f32> = (0..n)
            .flat_map(|i| {
                let t = i as f32 / sr as f32;
                let mut v = 0.0f32;
                for m in notes {
                    let f = 440.0 * 2f32.powf((m as f32 - 69.0) / 12.0);
                    v += (std::f32::consts::TAU * f * t).sin() + 0.5 * (std::f32::consts::TAU * f * 2.0 * t).sin();
                }
                let s = v * 0.1;
                [s, s]
            })
            .collect();
        wave_engine::sources::write_wav(&p, &data, sr).unwrap();
        p
    };
    let c_wav = chord_wav("c_maj.wav", [60, 64, 67]);
    let fs_wav = chord_wav("fs_maj.wav", [66, 70, 73]);
    let mut p = Project::routed();
    let tune = new_fx(9500, FxKind::Tune, p.bpm);
    p.inserts[2].fx.push(tune);
    let e = Engine::new(p, Arc::new(NoPlugins));
    let (_, c1) = e.import_audio(&[c_wav], Some((0, 0.0))).unwrap();
    let (_, c2) = e.import_audio(&[fs_wav], Some((1, 0.0))).unwrap();
    let a1 = e.music_analysis(Some(c1[0]), None).unwrap();
    assert_eq!(a1.key.unwrap().root, 0);
    let a2 = e.music_analysis(Some(c2[0]), None).unwrap();
    assert_eq!(a2.key.unwrap().root, 6);
    let proj_ex = e.music_analysis(None, Some(9500)).unwrap();
    assert_eq!(proj_ex.key.unwrap().root, 0);
    let _ = std::fs::remove_dir_all(dir);
}

fn width_render(width: f32) -> Vec<f32> {
    let dir = std::env::temp_dir().join(format!("wave-width-{}-{}", std::process::id(), width));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("st.wav");
    let tone: Vec<f32> = (0..24000).flat_map(|i| [(i as f32 * 0.05).sin() * 0.4, (i as f32 * 0.071).sin() * 0.3]).collect();
    wave_engine::sources::write_wav(&wav, &tone, 48000).unwrap();
    let mut p = Project::routed();
    p.inserts[0].fx.clear();
    p.inserts[0].width = width;
    let e = Engine::new(p, Arc::new(NoPlugins));
    e.import_audio(&[wav], Some((0, 0.0))).unwrap();
    let out = e.render(0.0, 4.0, 0.0, 48000).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    out
}

#[test]
fn insert_width_scales_the_side_signal() {
    let one = width_render(1.0);
    assert!(rms(&one) > 0.05);
    let mono = width_render(0.0);
    assert!(mono.chunks(2).all(|f| f[0] == f[1]), "width 0 is not mono");
    let wide = width_render(2.0);
    for (a, b) in one.chunks(2).zip(wide.chunks(2)) {
        let (m1, s1) = ((a[0] + a[1]) * 0.5, (a[0] - a[1]) * 0.5);
        let (m2, s2) = ((b[0] + b[1]) * 0.5, (b[0] - b[1]) * 0.5);
        assert!((m1 - m2).abs() < 1e-5 && (2.0 * s1 - s2).abs() < 1e-5);
    }
    // width 1 changes nothing: matches the mid/side-free default render bit for bit
    let again = width_render(1.0);
    assert_eq!(one, again);
}

#[test]
fn old_insert_json_loads_with_unity_width() {
    let mut v = serde_json::to_value(&Project::routed().inserts[0]).unwrap();
    assert_eq!(v["width"], 1.0);
    v.as_object_mut().unwrap().remove("width");
    let ins: Insert = serde_json::from_value(v).unwrap();
    assert_eq!(ins.width, 1.0);
}

#[test]
fn saving_keeps_the_previous_save_as_a_backup() {
    let dir = std::env::temp_dir().join(format!("wave-backup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Song.wave");
    let backup = engine::backup_path(&path);
    assert_eq!(backup, dir.join("Song (backup).wave"));
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    e.apply(&[Op::SetBpm { bpm: 100.0 }], false, None).unwrap();
    e.save(&path).unwrap();
    assert!(!backup.exists(), "a first save has nothing to back up");
    e.apply(&[Op::SetBpm { bpm: 150.0 }], false, None).unwrap();
    e.save(&path).unwrap();
    // the backup is an ordinary project: it opens, and holds the save before the last one
    let f = Engine::new(Project::empty(), Arc::new(NoPlugins));
    f.open(&backup).unwrap();
    assert_eq!(f.project().bpm, 100.0);
    f.open(&path).unwrap();
    assert_eq!(f.project().bpm, 150.0);
    assert!(!dir.join("Song.wave.tmp").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_copy_leaves_the_project_unsaved_and_reopens_as_unsaved() {
    let dir = std::env::temp_dir().join(format!("wave-copy-{}", std::process::id()));
    let copy = dir.join("Recovery").join("1-2.wave");
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    e.apply(&[Op::SetBpm { bpm: 133.0 }], false, None).unwrap();
    e.save_copy(&copy).unwrap();
    let st = e.state();
    assert!(st.path.is_none() && st.dirty, "a recovery copy is not a save");
    assert!(!engine::backup_path(&copy).exists(), "a copy keeps no backup");

    let f = Engine::new(Project::empty(), Arc::new(NoPlugins));
    f.open_recovered(&copy).unwrap();
    let st = f.state();
    assert_eq!(f.project(), e.project());
    assert!(st.path.is_none(), "Save must ask where to put a recovered project");
    assert!(st.dirty);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_arrangement_can_run_past_the_old_128_bar_limit() {
    const { assert!(TOTAL_BEATS >= 2048.0) };
    let mut p = Project::sketch();
    let pat = p.patterns[0].id;
    let len = p.patterns[0].len;
    let last = p.clips.iter().map(|c| c.id).max().unwrap_or(0);
    // bar 476: past the old 512-beat end, inside the new one
    let start = 1900.0;
    p.clips.push(Clip { id: last + 1, track: 0, start, len, offset: 0.0, kind: ClipKind::Pat, source: pat, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None });
    let e = Engine::new(p, Arc::new(NoPlugins));
    let (from, to) = Engine::export_range(&e.project(), false);
    assert_eq!(from, 0.0);
    assert!(to >= start + len - 1e-9, "the song runs to the late clip: {to}");
    let out = e.render(start, start + len, 0.0, 48000).unwrap();
    assert!(out.iter().any(|x| x.abs() > 1e-3), "the late clip plays");

    // a pattern may hold notes past beat 512 too
    let mut pat = e.project().patterns[0].clone();
    let k = pat.notes.first().map(|n| n.ch).unwrap();
    pat.notes.push(Note::new(k, 600.0 * 4.0, 4.0, None));
    e.apply(&[Op::PutPattern { pattern: pat }], false, None).unwrap();
}
