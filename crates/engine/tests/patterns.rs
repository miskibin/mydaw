//! Channels, the step sequencer and the piano roll: model, ops, migration and rendering.

use std::path::PathBuf;
use std::sync::Arc;

use wave_engine::*;

const SR: u32 = 48000;

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wave-pat-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An empty project (the starter kit in the rack), no master limiter, one pattern on track 0.
fn one_pattern(notes: Vec<Note>, len: f64, swing: f32) -> Project {
    let mut p = Project::routed();
    p.inserts[0].fx.clear();
    p.patterns = vec![Pattern { id: 1, name: "P".into(), color: "#4f8ef7".into(), len, min_len: len, notes, swing }];
    p.clips = vec![clip(1, 0, 0.0, len, 1)];
    p
}

fn clip(id: Id, track: u32, start: f64, len: f64, pat: Id) -> Clip {
    Clip {
        id,
        track,
        start,
        len,
        offset: 0.0,
        kind: ClipKind::Pat,
        source: pat,
        muted: false,
        gain: 0.0,
        fade_in: 0.0,
        fade_out: 0.0,
        pan: 0.0,
        norm: false,
        rev: false,
        tune: None,
        stretch: 1.0,
        resample: false,
        pitch: 0.0,
    }
}

/// A starter-kit channel by name (Kick, Clap, Hat, Snare).
fn chan(p: &Project, name: &str) -> Id {
    p.channels.iter().find(|c| c.name == name).unwrap().id
}

fn render(p: Project, beats: f64) -> Vec<f32> {
    Engine::new(p, Arc::new(NoPlugins)).render(0.0, beats, 0.0, SR).unwrap()
}

fn left(x: &[f32]) -> Vec<f32> {
    x.as_chunks::<2>().0.iter().map(|f| f[0]).collect()
}

/// First frame whose level exceeds `thr`.
fn onset(x: &[f32], thr: f32) -> Option<usize> {
    left(x).iter().position(|v| v.abs() > thr)
}

#[test]
fn new_projects_start_with_the_starter_kit_as_samplers() {
    let p = Project::routed();
    assert_eq!(p.channels.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Kick", "Clap", "Hat", "Snare"]);
    for (c, (file, _)) in p.channels.iter().zip(STARTER_KIT) {
        // every channel is a plain sampler of a standard-library file the project lists
        let ChannelSrc::Sample { source, sampler } = c.src else { panic!("{} is not a sampler", c.name) };
        assert_eq!(sampler, SamplerState::default());
        let src = p.sources.iter().find(|s| s.id == source).unwrap();
        assert_eq!(src.path, format!("lib:Standard/{file}"));
        assert!(c.vol == VOL_UNITY && c.pan == 0.0 && c.insert.is_none() && c.root == SAMPLE_ROOT);
    }
    assert!(p.next_id > p.channels.iter().map(|c| c.id).chain(p.sources.iter().map(|s| s.id)).max().unwrap());
    // and they sound without anything installed on the computer
    let k = chan(&p, "Kick");
    assert!(rms(&render(one_pattern(vec![Note::new(k, 0.0, 1.0, None)], 4.0, 0.0), 4.0)) > 0.01);
}

#[test]
fn velocity_scales_the_output() {
    for inst in ["Kick", "Snare"] {
        let p0 = Project::routed();
        let k = chan(&p0, inst);
        let loud = render(one_pattern(vec![Note::new(k, 0.0, 4.0, None)], 4.0, 0.0), 4.0);
        let mut n = Note::new(k, 0.0, 4.0, None);
        n.v = 0.5;
        let soft = render(one_pattern(vec![n], 4.0, 0.0), 4.0);
        let ratio = rms(&soft) / rms(&loud);
        assert!((ratio - 0.5).abs() < 1e-4, "{inst:?}: velocity 0.5 gave level ratio {ratio}");
        let mut n = Note::new(k, 0.0, 4.0, None);
        n.v = 0.0;
        assert!(rms(&render(one_pattern(vec![n], 4.0, 0.0), 4.0)) < 1e-9);
    }
}

#[test]
fn swing_moves_only_odd_sixteenths() {
    // the mapping itself
    for s in 0..32 {
        let off = swing_offset(s as f32, 1.0);
        if s % 2 == 1 {
            assert_eq!(off, 0.5, "step {s}");
        } else {
            assert_eq!(off, 0.0, "step {s}");
        }
        assert_eq!(swing_offset(s as f32, 0.0), 0.0);
        assert_eq!(swing_offset(s as f32 + 0.25, 1.0), 0.0, "off-grid notes stay");
    }
    assert!((swing_offset(3.0, 0.5) - 0.25).abs() < 1e-6);
    // and in the render: one kick per pattern, at step s, with 100 % swing
    let spb = 60.0 / 128.0;
    let step_frames = spb / 4.0 * SR as f64;
    for s in [0u32, 1, 2, 3, 6, 7] {
        let p0 = Project::routed();
        let k = chan(&p0, "Kick");
        let out = render(one_pattern(vec![Note::new(k, s as f32, 1.0, None)], 4.0, 1.0), 4.0);
        let at = onset(&out, 1e-4).unwrap() as f64;
        let want = (s as f64 + if s % 2 == 1 { 0.5 } else { 0.0 }) * step_frames;
        assert!((at - want).abs() <= 2.0, "step {s}: onset at {at}, expected {want}");
    }
}

#[test]
fn files_with_the_old_synthesised_instruments_open_as_samplers() {
    // a file written before channels existed: its notes say `inst` (kick, clap, hat, bass)
    let dir = tmp("migrate");
    let path = dir.join("demo_v1.wave");
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/demo_v1.wave"), &path).unwrap();
    let e = Engine::new(Project::routed(), Arc::new(NoPlugins));
    assert!(e.open(&path).unwrap().is_empty());
    let p = e.project();
    // one sampler channel per instrument the song used, each note on its channel
    assert_eq!(p.channels.len(), 4);
    assert!(p.channels.iter().all(|c| matches!(c.src, ChannelSrc::Sample { .. })));
    let notes: Vec<&Note> = p.patterns.iter().flat_map(|pt| &pt.notes).collect();
    assert_eq!(notes.len(), 23);
    assert!(notes.iter().all(|n| p.channel(n.ch).is_some()));
    // the bass held its notes: it is the one gated sampler
    let gated: Vec<bool> = p.channels.iter().map(|c| matches!(c.src, ChannelSrc::Sample { sampler, .. } if sampler.gate)).collect();
    assert_eq!(gated.iter().filter(|g| **g).count(), 1);
    assert!(rms(&e.render(0.0, 32.0, 0.0, SR).unwrap()) > 0.01);
    // a file with `"kind":"inst"` channels (after channels, before the standard library)
    let mut doc: serde_json::Value = serde_json::to_value(Project::routed()).unwrap();
    doc["channels"] = serde_json::json!([
        { "id": 1000, "name": "My Kick", "color": "#4f8ef7", "kind": "inst", "inst": "kick", "vol": 0.5, "pan": -0.25, "root": 60 },
        { "id": 1001, "name": "Bass", "color": "#7c8cf8", "kind": "inst", "inst": "bass", "root": 36 },
    ]);
    doc["sources"] = serde_json::json!([]);
    doc["patterns"] = serde_json::json!([{ "id": 1, "name": "P", "color": "#fff", "len": 4, "notes": [{ "ch": 1000, "step": 0, "len": 1 }, { "ch": 1001, "step": 4, "len": 4, "p": 33 }] }]);
    let q = Project::from_json(doc).unwrap();
    let kick = q.channel(1000).unwrap();
    // the channel keeps its name and mix; it now plays a sample the project lists
    assert_eq!((kick.name.as_str(), kick.vol, kick.pan), ("My Kick", 0.5, -0.25));
    let src = q.sources.iter().find(|s| Some(s.id) == kick.sample_source()).unwrap();
    assert!(src.path.starts_with("lib:Standard/kick"));
    assert!(q.next_id > q.sources.iter().map(|s| s.id).max().unwrap());
    assert_eq!(q.patterns[0].notes.len(), 2);
    // and reading it again changes nothing
    assert_eq!(Project::from_json(serde_json::to_value(&q).unwrap()).unwrap(), q);
}

#[test]
fn notes_of_unknown_channels_are_refused() {
    let e = Engine::new(Project::routed(), Arc::new(NoPlugins));
    let bad = Pattern {
        id: 8,
        name: "x".into(),
        color: "#fff".into(),
        len: 4.0,
        min_len: 4.0,
        notes: vec![Note::new(424242, 0.0, 1.0, None)],
        swing: 0.0,
    };
    assert!(e.apply(&[Op::PutPattern { pattern: bad }], false, None).is_err());
}

/// A 1 s mono sine at `hz` written to a WAV.
fn sine_wav(path: &std::path::Path, hz: f64) {
    let spec = hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..SR {
        w.write_sample((i as f64 / SR as f64 * hz * std::f64::consts::TAU).sin() as f32 * 0.5).unwrap();
    }
    w.finalize().unwrap();
}

/// Frequency from positive-going zero crossings of `x[a..b]`.
fn freq(x: &[f32], a: usize, b: usize) -> f64 {
    let s = &x[a..b];
    let ups: Vec<usize> = (1..s.len()).filter(|&i| s[i - 1] <= 0.0 && s[i] > 0.0).collect();
    (ups.len() - 1) as f64 / ((ups[ups.len() - 1] - ups[0]) as f64 / SR as f64)
}

#[test]
fn sampler_channel_plays_its_sample_at_the_note_pitch() {
    let dir = tmp("sampler");
    let wav = dir.join("a440.wav");
    sine_wav(&wav, 440.0);
    let e = Engine::new(Project::routed(), Arc::new(NoPlugins));
    let (srcs, _) = e.import_audio(&[wav], None).unwrap();
    let p = e.project();
    let id = p.next_id;
    let ch = Channel {
        id,
        name: "A440".into(),
        color: "#47b8a8".into(),
        src: ChannelSrc::Sample { source: srcs[0], sampler: SamplerState::default() },
        vol: VOL_UNITY,
        pan: 0.0,
        mute: false,
        solo: false,
        insert: None,
        root: SAMPLE_ROOT,
    };
    // no master limiter: a clean measurement
    let lim = p.inserts[0].fx.iter().map(|f| Op::RemoveFx { fx_id: f.id });
    e.apply(&lim.chain([Op::PutChannel { channel: ch, index: None }]).collect::<Vec<_>>(), false, None).unwrap();
    let cases = [
        (None, 440.0),
        (Some(SAMPLE_ROOT + 12), 880.0),
        (Some(SAMPLE_ROOT - 12), 220.0),
        (Some(SAMPLE_ROOT + 7), 440.0 * 2f64.powf(7.0 / 12.0)),
    ];
    for (k, (key, hz)) in cases.into_iter().enumerate() {
        let pat = Pattern {
            id: 50 + k as Id,
            name: "s".into(),
            color: "#fff".into(),
            len: 4.0,
            min_len: 4.0,
            notes: vec![Note::new(id, 0.0, 1.0, key)],
            swing: 0.0,
        };
        let c = clip(60 + k as Id, 0, 0.0, 4.0, pat.id);
        e.apply(
            &[Op::RemoveClips { ids: (60..70).collect() }, Op::PutPattern { pattern: pat }, Op::PutClips { clips: vec![c] }],
            false,
            None,
        )
        .unwrap();
        let out = left(&e.render(0.0, 1.0, 0.0, SR).unwrap());
        let got = freq(&out, 2000, 12000);
        assert!((got / hz - 1.0).abs() < 0.003, "key {key:?}: {got} Hz, want {hz}");
        // one-shot at unity: the level of the file (0.5 peak), not more
        let pk = out[2000..12000].iter().fold(0f32, |a, b| a.max(b.abs()));
        assert!((pk - 0.5).abs() < 0.01, "peak {pk}");
    }
}

#[test]
fn pattern_length_follows_its_notes_and_whole_clips_follow_it() {
    let p0 = Project::routed();
    let k = chan(&p0, "Kick");
    // a length sent by the caller is ignored: the notes reach into bar 2
    let mut p = one_pattern(vec![Note::new(k, 0.0, 1.0, None), Note::new(k, 20.0, 1.0, None)], 4.0, 0.0);
    p.clips[0].len = 8.0;
    p.clips.push(Clip { len: 16.0, ..clip(2, 1, 16.0, 16.0, 1) }); // two passes: sized on purpose
    p.clips.push(Clip { offset: 4.0, len: 4.0, ..clip(3, 2, 40.0, 4.0, 1) }); // second half only
    let e = Engine::new(p, Arc::new(NoPlugins));
    assert_eq!(e.project().patterns[0].len, 8.0, "derived on load from the last note");
    // deleting the note in bar 2: the pattern shrinks to one bar
    let mut pat = e.project().patterns[0].clone();
    pat.notes.truncate(1);
    pat.len = 64.0;
    e.apply(&[Op::PutPattern { pattern: pat.clone() }], false, None).unwrap();
    let p = e.project();
    let len = |p: &Project, id: Id| p.clips.iter().find(|c| c.id == id).unwrap().len;
    assert_eq!(p.patterns[0].len, 4.0);
    assert_eq!(len(&p, 1), 4.0, "a clip showing the whole pattern follows it");
    assert_eq!(len(&p, 2), 16.0);
    assert_eq!(len(&p, 3), 4.0);
    // "+ bar": an empty second bar kept on purpose (minLen), so the kick plays every 2 bars
    pat.min_len = 8.0;
    e.apply(&[Op::PutPattern { pattern: pat.clone() }], false, None).unwrap();
    assert_eq!(e.project().patterns[0].len, 8.0);
    assert_eq!(len(&e.project(), 1), 8.0);
    let out = left(&e.render(0.0, 8.0, 0.0, SR).unwrap());
    let spb = 60.0 / 128.0;
    let bar2 = (4.0 * spb * SR as f64) as usize;
    assert!(out[bar2..bar2 + 4000].iter().all(|v| v.abs() < 1e-6), "the empty bar is silent");
    // a note past the end grows it; minLen is whole bars and at least one
    pat.min_len = 0.5;
    pat.notes.push(Note::new(k, 40.0, 4.0, None));
    e.apply(&[Op::PutPattern { pattern: pat }], false, None).unwrap();
    let pt = &e.project().patterns[0];
    assert_eq!((pt.len, pt.min_len), (12.0, 4.0));
    // undo restores the length and the clip with it
    assert!(e.undo());
    assert_eq!(e.project().patterns[0].len, 8.0);
    assert_eq!(e.project().clips[0].len, 8.0);
}

#[test]
fn removing_a_pattern_removes_its_clips_and_undoes() {
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    let before = e.project();
    let id = before.patterns[0].id;
    assert!(before.clips.iter().any(|c| c.kind == ClipKind::Pat && c.source == id));
    e.apply(&[Op::RemovePattern { id }], false, None).unwrap();
    let p = e.project();
    assert!(p.patterns.iter().all(|x| x.id != id));
    assert!(!p.clips.iter().any(|c| c.kind == ClipKind::Pat && c.source == id));
    assert!(e.apply(&[Op::RemovePattern { id }], false, None).is_err(), "an unknown pattern is refused");
    assert!(e.undo());
    assert_eq!(e.project(), before);
}

#[test]
fn notes_past_the_song_end_are_refused() {
    let p0 = Project::routed();
    let k = chan(&p0, "Kick");
    let e = Engine::new(one_pattern(vec![], 4.0, 0.0), Arc::new(NoPlugins));
    let mut pat = e.project().patterns[0].clone();
    pat.notes.push(Note::new(k, (TOTAL_BEATS * 4.0) as f32 - 1.0, 2.0, None));
    assert!(e.apply(&[Op::PutPattern { pattern: pat }], false, None).is_err());
}

#[test]
fn step_edits_undo_and_redo() {
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    let before = e.project();
    let mut pat = before.patterns[0].clone();
    let kick = chan(&before, "Kick");
    pat.notes.push(Note::new(kick, 6.0, 1.0, None));
    e.apply(&[Op::PutPattern { pattern: pat.clone() }], false, None).unwrap();
    pat.notes.retain(|n| n.step != 0.0);
    e.apply(&[Op::PutPattern { pattern: pat.clone() }], false, None).unwrap();
    // a velocity drag: many transient edits, one undo step
    e.begin_gesture();
    for v in [0.9, 0.7, 0.5] {
        pat.notes[0].v = v;
        e.apply(&[Op::PutPattern { pattern: pat.clone() }], true, None).unwrap();
    }
    e.end_gesture();
    assert_eq!(e.project().patterns[0].notes[0].v, 0.5);
    assert!(e.undo());
    assert_eq!(e.project().patterns[0].notes[0].v, 1.0);
    assert!(e.undo());
    assert!(e.project().patterns[0].notes.iter().any(|n| n.step == 0.0));
    assert!(e.undo());
    assert_eq!(e.project(), before);
    assert!(e.redo());
    assert!(e.project().patterns[0].notes.iter().any(|n| n.step == 6.0));
}

#[test]
fn channels_mute_solo_route_and_remove() {
    let p0 = Project::routed();
    let (k, h) = (chan(&p0, "Kick"), chan(&p0, "Hat"));
    let p = one_pattern(vec![Note::new(k, 0.0, 1.0, None), Note::new(h, 4.0, 1.0, None)], 4.0, 0.0);
    let e = Engine::new(p, Arc::new(NoPlugins));
    let full = rms(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    let mut kc = e.project().channel(k).unwrap().clone();
    kc.mute = true;
    e.apply(&[Op::PutChannel { channel: kc.clone(), index: None }], false, None).unwrap();
    let hat_only = rms(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    assert!(hat_only < full * 0.5 && hat_only > 0.0);
    kc.mute = false;
    kc.solo = true;
    e.apply(&[Op::PutChannel { channel: kc.clone(), index: None }], false, None).unwrap();
    let kick_only = rms(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    assert!(kick_only > hat_only && kick_only < full);
    kc.solo = false;
    // routed to insert 3 and that insert muted: silence for the kick, the hat still plays (track 0 → insert 1)
    kc.insert = Some(3);
    let mut ins = e.project().inserts[3].clone();
    ins.muted = true;
    e.apply(&[Op::PutChannel { channel: kc.clone(), index: None }, Op::PutInsert { insert: ins }], false, None).unwrap();
    // (the hat is noise: a different seed, same level)
    let routed = rms(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    assert!((routed / hat_only - 1.0).abs() < 0.15, "{routed} vs {hat_only}");
    // pan hard left: nothing on the right
    kc.insert = None;
    kc.pan = -1.0;
    let mut hc = e.project().channel(h).unwrap().clone();
    hc.mute = true;
    e.apply(&[Op::PutChannel { channel: kc, index: None }, Op::PutChannel { channel: hc, index: None }], false, None).unwrap();
    let out = e.render(0.0, 4.0, 0.0, SR).unwrap();
    let r: Vec<f32> = out.as_chunks::<2>().0.iter().map(|f| f[1]).collect();
    assert!(rms(&left(&out)) > 0.01 && rms(&r) < 1e-9);
    // removing a channel removes its notes everywhere
    e.apply(&[Op::RemoveChannel { id: k }], false, None).unwrap();
    assert!(e.project().patterns[0].notes.iter().all(|n| n.ch != k));
    assert!(e.apply(&[Op::RemoveChannel { id: k }], false, None).is_err());
    // invalid channels are refused
    let mut bad = e.project().channels[0].clone();
    bad.insert = Some(999);
    assert!(e.apply(&[Op::PutChannel { channel: bad, index: None }], false, None).is_err());
}

#[test]
fn save_and_open_round_trip() {
    let dir = tmp("roundtrip");
    let wav = dir.join("tone.wav");
    sine_wav(&wav, 330.0);
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    let (srcs, _) = e.import_audio(&[wav], None).unwrap();
    let p = e.project();
    let id = p.next_id;
    let ch = Channel {
        id,
        name: "Tone".into(),
        color: "#47b8a8".into(),
        src: ChannelSrc::Sample { source: srcs[0], sampler: SamplerState::default() },
        vol: 0.6,
        pan: 0.25,
        mute: false,
        solo: true,
        insert: Some(4),
        root: 57,
    };
    let mut pat = p.patterns[0].clone();
    pat.swing = 0.4;
    pat.notes[0].v = 0.35;
    pat.notes.push(Note { v: 0.8, ..Note::new(id, 3.0, 2.5, Some(64)) });
    e.apply(&[Op::PutChannel { channel: ch, index: Some(1) }, Op::PutPattern { pattern: pat }], false, None).unwrap();
    let path = dir.join("song.wave");
    e.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"channels\"") && !text.contains("\"inst\": \"kick\",\n          \"step\""));
    let e2 = Engine::new(Project::routed(), Arc::new(NoPlugins));
    assert!(e2.open(&path).unwrap().is_empty());
    assert_eq!(e2.project(), e.project());
    assert_eq!(e2.project().channels[1].name, "Tone");
    let a = e.render(0.0, 8.0, 0.5, SR).unwrap();
    let b = e2.render(0.0, 8.0, 0.5, SR).unwrap();
    assert!(a == b, "the reopened song sounds different");
}

#[test]
fn pattern_preview_and_auditions_are_not_edits() {
    let e = Engine::new(Project::sketch(), Arc::new(NoPlugins));
    let r = e.revision();
    let k = e.project().channels[0].id;
    e.note_audition(k, None, 0.8, Some(1)).unwrap();
    e.pattern_play(Some(1)).unwrap();
    e.pattern_play(None).unwrap();
    assert_eq!(e.revision(), r);
    assert!(e.note_audition(424242, None, 1.0, None).is_err());
    assert!(e.pattern_play(Some(424242)).is_err());
}
