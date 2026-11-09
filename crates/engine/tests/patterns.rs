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

/// An empty project (four built-in channels), no master limiter, one pattern on track 0.
fn one_pattern(notes: Vec<Note>, len: f64, swing: f32) -> Project {
    let mut p = Project::empty();
    p.inserts[0].fx.clear();
    p.patterns = vec![Pattern {
        id: 1,
        name: "P".into(),
        color: "#4f8ef7".into(),
        len,
        notes,
        swing,
    }];
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
    }
}

fn chan(p: &Project, inst: Inst) -> Id {
    p.channels
        .iter()
        .find(|c| c.src == ChannelSrc::Inst { inst })
        .unwrap()
        .id
}

fn render(p: Project, beats: f64) -> Vec<f32> {
    Engine::new(p, Arc::new(NoPlugins))
        .render(0.0, beats, 0.0, SR)
        .unwrap()
}

fn left(x: &[f32]) -> Vec<f32> {
    x.chunks_exact(2).map(|f| f[0]).collect()
}

/// First frame whose level exceeds `thr`.
fn onset(x: &[f32], thr: f32) -> Option<usize> {
    left(x).iter().position(|v| v.abs() > thr)
}

#[test]
fn new_projects_have_the_four_builtin_channels() {
    let p = Project::empty();
    let kinds: Vec<_> = p.channels.iter().map(|c| c.src.clone()).collect();
    assert_eq!(
        kinds,
        Inst::ALL
            .iter()
            .map(|&inst| ChannelSrc::Inst { inst })
            .collect::<Vec<_>>()
    );
    assert!(p
        .channels
        .iter()
        .all(|c| c.vol == VOL_UNITY && c.pan == 0.0 && c.insert.is_none()));
    assert!(p.next_id > p.channels.iter().map(|c| c.id).max().unwrap());
}

#[test]
fn velocity_scales_the_output() {
    for inst in [Inst::Kick, Inst::Bass] {
        let p0 = Project::empty();
        let k = chan(&p0, inst);
        let loud = render(
            one_pattern(vec![Note::new(k, 0.0, 4.0, None)], 4.0, 0.0),
            4.0,
        );
        let mut n = Note::new(k, 0.0, 4.0, None);
        n.v = 0.5;
        let soft = render(one_pattern(vec![n], 4.0, 0.0), 4.0);
        let ratio = rms(&soft) / rms(&loud);
        assert!(
            (ratio - 0.5).abs() < 1e-4,
            "{inst:?}: velocity 0.5 gave level ratio {ratio}"
        );
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
        assert_eq!(
            swing_offset(s as f32 + 0.25, 1.0),
            0.0,
            "off-grid notes stay"
        );
    }
    assert!((swing_offset(3.0, 0.5) - 0.25).abs() < 1e-6);
    // and in the render: one kick per pattern, at step s, with 100 % swing
    let spb = 60.0 / 128.0;
    let step_frames = spb / 4.0 * SR as f64;
    for s in [0u32, 1, 2, 3, 6, 7] {
        let p0 = Project::empty();
        let k = chan(&p0, Inst::Kick);
        let out = render(
            one_pattern(vec![Note::new(k, s as f32, 1.0, None)], 4.0, 1.0),
            4.0,
        );
        let at = onset(&out, 1e-4).unwrap() as f64;
        let want = (s as f64 + if s % 2 == 1 { 0.5 } else { 0.0 }) * step_frames;
        assert!(
            (at - want).abs() <= 2.0,
            "step {s}: onset at {at}, expected {want}"
        );
    }
}

#[test]
fn old_projects_migrate_and_render_like_before() {
    // a file written by the engine before channels existed (notes say `inst`)
    let dir = tmp("migrate");
    let path = dir.join("demo_v1.wave");
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/demo_v1.wave"),
        &path,
    )
    .unwrap();
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    assert!(e.open(&path).unwrap().is_empty());
    let migrated = e.project();
    // the migration is exactly the new demo: built-in channels in Kick, Clap, Hat, Bass order.
    // Reverb and limiter settings are the exception: their parameter sets and factory
    // presets changed with the v2 DSP after the fixture was written, and old files keep theirs.
    let mut expected = Project::demo();
    for (m, d) in migrated.inserts.iter().zip(expected.inserts.iter_mut()) {
        for (fm, fd) in m.fx.iter().zip(d.fx.iter_mut()) {
            if fm.kind == FxKind::Reverb || fm.kind == FxKind::Limit {
                fd.params = fm.params.clone();
                fd.preset = fm.preset;
            }
            // parameters added later (Comp "det") are absent from old files; they read as
            // their default, which is the old behaviour
            if fm.kind == FxKind::Comp && !fm.params.contains_key("det") {
                fd.params.remove("det");
            }
        }
    }
    assert_eq!(migrated, expected);
    assert!(migrated
        .patterns
        .iter()
        .flat_map(|p| &p.notes)
        .all(|n| n.ch != 0 && n.inst.is_none()));
    // the render matches the one made by the old engine (RMS per 20 ms)
    let out = e.render(0.0, 32.0, 1.0, SR).unwrap();
    let fx: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/demo_v1_render.json")).unwrap();
    assert_eq!(out.len() / 2, fx["frames"].as_u64().unwrap() as usize);
    let w = fx["window"].as_u64().unwrap() as usize;
    for (i, (chunk, want)) in out
        .chunks(w * 2)
        .zip(fx["rms"].as_array().unwrap())
        .enumerate()
    {
        let (mut l, mut r) = (0f64, 0f64);
        for f in chunk.chunks_exact(2) {
            l += (f[0] as f64).powi(2);
            r += (f[1] as f64).powi(2);
        }
        let n = (chunk.len() / 2) as f64;
        for (got, want) in [
            ((l / n).sqrt(), want[0].as_f64().unwrap()),
            ((r / n).sqrt(), want[1].as_f64().unwrap()),
        ] {
            assert!(
                (got - want).abs() <= 1e-6 + want * 1e-5,
                "window {i}: rms {got} vs {want} before channels"
            );
        }
    }
    // and the in-memory demo renders bit-identically to the migrated file
    let demo = Engine::new(expected, Arc::new(NoPlugins))
        .render(0.0, 32.0, 1.0, SR)
        .unwrap();
    assert!(demo == out, "demo and migrated file differ");
    // migrating twice changes nothing
    let mut again = migrated.clone();
    again.migrate();
    assert_eq!(again, migrated);
}

#[test]
fn legacy_inst_notes_through_put_pattern_get_a_channel() {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    e.apply(
        &[Op::RemoveChannel {
            id: chan(&e.project(), Inst::Hat),
        }],
        false,
        None,
    )
    .unwrap();
    let ops: Vec<Op> = serde_json::from_value(serde_json::json!([{ "op": "putPattern", "pattern": {
        "id": 7, "name": "Hats", "color": "#e0a44a", "len": 4, "notes": [{"inst": "hat", "step": 0, "len": 1}, {"inst": "kick", "step": 4, "len": 1}] } }]))
    .unwrap();
    e.apply(&ops, false, None).unwrap();
    let p = e.project();
    let hat = chan(&p, Inst::Hat);
    let pat = p.patterns.iter().find(|x| x.id == 7).unwrap();
    assert_eq!(pat.notes[0].ch, hat);
    assert_eq!(pat.notes[1].ch, chan(&p, Inst::Kick));
    // the JSON never says `inst` again
    assert!(!serde_json::to_string(&p)
        .unwrap()
        .contains("\"inst\":\"hat\",\"step\""));
    // a note without a channel or with an unknown one is refused
    let bad = Pattern {
        id: 8,
        name: "x".into(),
        color: "#fff".into(),
        len: 4.0,
        notes: vec![Note::new(424242, 0.0, 1.0, None)],
        swing: 0.0,
    };
    assert!(e
        .apply(&[Op::PutPattern { pattern: bad }], false, None)
        .is_err());
}

/// A 1 s mono sine at `hz` written to a WAV.
fn sine_wav(path: &std::path::Path, hz: f64) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SR,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..SR {
        w.write_sample((i as f64 / SR as f64 * hz * std::f64::consts::TAU).sin() as f32 * 0.5)
            .unwrap();
    }
    w.finalize().unwrap();
}

/// Frequency from positive-going zero crossings of `x[a..b]`.
fn freq(x: &[f32], a: usize, b: usize) -> f64 {
    let s = &x[a..b];
    let ups: Vec<usize> = (1..s.len())
        .filter(|&i| s[i - 1] <= 0.0 && s[i] > 0.0)
        .collect();
    (ups.len() - 1) as f64 / ((ups[ups.len() - 1] - ups[0]) as f64 / SR as f64)
}

#[test]
fn sampler_channel_plays_its_sample_at_the_note_pitch() {
    let dir = tmp("sampler");
    let wav = dir.join("a440.wav");
    sine_wav(&wav, 440.0);
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    let (srcs, _) = e.import_audio(&[wav], None).unwrap();
    let p = e.project();
    let id = p.next_id;
    let ch = Channel {
        id,
        name: "A440".into(),
        color: "#47b8a8".into(),
        src: ChannelSrc::Sample { source: srcs[0] },
        vol: VOL_UNITY,
        pan: 0.0,
        mute: false,
        solo: false,
        insert: None,
        root: SAMPLE_ROOT,
    };
    // no master limiter: a clean measurement
    let lim = p.inserts[0].fx.iter().map(|f| Op::RemoveFx { fx_id: f.id });
    e.apply(
        &lim.chain([Op::PutChannel {
            channel: ch,
            index: None,
        }])
        .collect::<Vec<_>>(),
        false,
        None,
    )
    .unwrap();
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
            notes: vec![Note::new(id, 0.0, 1.0, key)],
            swing: 0.0,
        };
        let c = clip(60 + k as Id, 0, 0.0, 4.0, pat.id);
        e.apply(
            &[
                Op::RemoveClips {
                    ids: (60..70).collect(),
                },
                Op::PutPattern { pattern: pat },
                Op::PutClips { clips: vec![c] },
            ],
            false,
            None,
        )
        .unwrap();
        let out = left(&e.render(0.0, 1.0, 0.0, SR).unwrap());
        let got = freq(&out, 2000, 12000);
        assert!(
            (got / hz - 1.0).abs() < 0.003,
            "key {key:?}: {got} Hz, want {hz}"
        );
        // one-shot at unity: the level of the file (0.5 peak), not more
        let pk = out[2000..12000].iter().fold(0f32, |a, b| a.max(b.abs()));
        assert!((pk - 0.5).abs() < 0.01, "peak {pk}");
    }
}

#[test]
fn pattern_length_changes_follow_whole_clips_and_hide_late_notes() {
    let p0 = Project::empty();
    let k = chan(&p0, Inst::Kick);
    let mut p = one_pattern(
        vec![Note::new(k, 0.0, 1.0, None), Note::new(k, 20.0, 1.0, None)],
        8.0,
        0.0,
    );
    p.clips.push(Clip {
        len: 16.0,
        ..clip(2, 1, 16.0, 16.0, 1)
    }); // two passes: sized on purpose
    p.clips.push(Clip {
        offset: 4.0,
        len: 4.0,
        ..clip(3, 2, 40.0, 4.0, 1)
    }); // second half only
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut pat = e.project().patterns[0].clone();
    pat.len = 4.0;
    e.apply(
        &[Op::PutPattern {
            pattern: pat.clone(),
        }],
        false,
        None,
    )
    .unwrap();
    let p = e.project();
    let len = |id: Id| p.clips.iter().find(|c| c.id == id).unwrap().len;
    assert_eq!(len(1), 4.0, "a clip showing the whole pattern follows it");
    assert_eq!(len(2), 16.0);
    assert_eq!(len(3), 4.0);
    // the note at step 20 is kept, but neither played nor looped back in
    assert_eq!(p.patterns[0].notes.len(), 2);
    let out = left(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    let spb = 60.0 / 128.0;
    let second = (1.0 * spb * SR as f64) as usize; // after the first kick has died away
    assert!(
        out[second..].iter().all(|v| v.abs() < 1e-6),
        "a note past the end played"
    );
    // growing it again brings the note (and the clip length) back
    pat.len = 8.0;
    e.apply(&[Op::PutPattern { pattern: pat }], false, None)
        .unwrap();
    assert_eq!(e.project().clips[0].len, 8.0);
    let out = left(&e.render(0.0, 8.0, 0.0, SR).unwrap());
    let at = (5.0 * spb * SR as f64) as usize;
    assert!(out[at..at + 2000].iter().any(|v| v.abs() > 0.1));
    // undo restores the clip with the pattern
    assert!(e.undo());
    assert_eq!(e.project().clips[0].len, 4.0);
}

#[test]
fn step_edits_undo_and_redo() {
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    let before = e.project();
    let mut pat = before.patterns[0].clone();
    let kick = chan(&before, Inst::Kick);
    pat.notes.push(Note::new(kick, 6.0, 1.0, None));
    e.apply(
        &[Op::PutPattern {
            pattern: pat.clone(),
        }],
        false,
        None,
    )
    .unwrap();
    pat.notes.retain(|n| n.step != 0.0);
    e.apply(
        &[Op::PutPattern {
            pattern: pat.clone(),
        }],
        false,
        None,
    )
    .unwrap();
    // a velocity drag: many transient edits, one undo step
    e.begin_gesture();
    for v in [0.9, 0.7, 0.5] {
        pat.notes[0].v = v;
        e.apply(
            &[Op::PutPattern {
                pattern: pat.clone(),
            }],
            true,
            None,
        )
        .unwrap();
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
    let p0 = Project::empty();
    let (k, h) = (chan(&p0, Inst::Kick), chan(&p0, Inst::Hat));
    let p = one_pattern(
        vec![Note::new(k, 0.0, 1.0, None), Note::new(h, 4.0, 1.0, None)],
        4.0,
        0.0,
    );
    let e = Engine::new(p, Arc::new(NoPlugins));
    let full = rms(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    let mut kc = e.project().channel(k).unwrap().clone();
    kc.mute = true;
    e.apply(
        &[Op::PutChannel {
            channel: kc.clone(),
            index: None,
        }],
        false,
        None,
    )
    .unwrap();
    let hat_only = rms(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    assert!(hat_only < full * 0.5 && hat_only > 0.0);
    kc.mute = false;
    kc.solo = true;
    e.apply(
        &[Op::PutChannel {
            channel: kc.clone(),
            index: None,
        }],
        false,
        None,
    )
    .unwrap();
    let kick_only = rms(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    assert!(kick_only > hat_only && kick_only < full);
    kc.solo = false;
    // routed to insert 3 and that insert muted: silence for the kick, the hat still plays (track 0 → insert 1)
    kc.insert = Some(3);
    let mut ins = e.project().inserts[3].clone();
    ins.muted = true;
    e.apply(
        &[
            Op::PutChannel {
                channel: kc.clone(),
                index: None,
            },
            Op::PutInsert { insert: ins },
        ],
        false,
        None,
    )
    .unwrap();
    // (the hat is noise: a different seed, same level)
    let routed = rms(&e.render(0.0, 4.0, 0.0, SR).unwrap());
    assert!(
        (routed / hat_only - 1.0).abs() < 0.15,
        "{routed} vs {hat_only}"
    );
    // pan hard left: nothing on the right
    kc.insert = None;
    kc.pan = -1.0;
    let mut hc = e.project().channel(h).unwrap().clone();
    hc.mute = true;
    e.apply(
        &[
            Op::PutChannel {
                channel: kc,
                index: None,
            },
            Op::PutChannel {
                channel: hc,
                index: None,
            },
        ],
        false,
        None,
    )
    .unwrap();
    let out = e.render(0.0, 4.0, 0.0, SR).unwrap();
    let r: Vec<f32> = out.chunks_exact(2).map(|f| f[1]).collect();
    assert!(rms(&left(&out)) > 0.01 && rms(&r) < 1e-9);
    // removing a channel removes its notes everywhere
    e.apply(&[Op::RemoveChannel { id: k }], false, None)
        .unwrap();
    assert!(e.project().patterns[0].notes.iter().all(|n| n.ch != k));
    assert!(e
        .apply(&[Op::RemoveChannel { id: k }], false, None)
        .is_err());
    // invalid channels are refused
    let mut bad = e.project().channels[0].clone();
    bad.insert = Some(999);
    assert!(e
        .apply(
            &[Op::PutChannel {
                channel: bad,
                index: None
            }],
            false,
            None
        )
        .is_err());
}

#[test]
fn save_and_open_round_trip() {
    let dir = tmp("roundtrip");
    let wav = dir.join("tone.wav");
    sine_wav(&wav, 330.0);
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    let (srcs, _) = e.import_audio(&[wav], None).unwrap();
    let p = e.project();
    let id = p.next_id;
    let ch = Channel {
        id,
        name: "Tone".into(),
        color: "#47b8a8".into(),
        src: ChannelSrc::Sample { source: srcs[0] },
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
    pat.notes.push(Note {
        v: 0.8,
        ..Note::new(id, 3.0, 2.5, Some(64))
    });
    e.apply(
        &[
            Op::PutChannel {
                channel: ch,
                index: Some(1),
            },
            Op::PutPattern { pattern: pat },
        ],
        false,
        None,
    )
    .unwrap();
    let path = dir.join("song.wave");
    e.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("\"channels\"") && !text.contains("\"inst\": \"kick\",\n          \"step\"")
    );
    let e2 = Engine::new(Project::empty(), Arc::new(NoPlugins));
    assert!(e2.open(&path).unwrap().is_empty());
    assert_eq!(e2.project(), e.project());
    assert_eq!(e2.project().channels[1].name, "Tone");
    let a = e.render(0.0, 8.0, 0.5, SR).unwrap();
    let b = e2.render(0.0, 8.0, 0.5, SR).unwrap();
    assert!(a == b, "the reopened song sounds different");
}

#[test]
fn pattern_preview_and_auditions_are_not_edits() {
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    let r = e.revision();
    let k = e.project().channels[0].id;
    e.note_audition(k, None, 0.8, Some(1)).unwrap();
    e.pattern_play(Some(1)).unwrap();
    e.pattern_play(None).unwrap();
    assert_eq!(e.revision(), r);
    assert!(e.note_audition(424242, None, 1.0, None).is_err());
    assert!(e.pattern_play(Some(424242)).is_err());
}

/// Rewrites `fixtures/demo_v1_render.json` after an intended DSP change of a built-in effect
/// the demo uses: `cargo test -p wave-engine --test patterns regenerate_demo_v1_render -- --ignored`.
#[test]
#[ignore]
fn regenerate_demo_v1_render() {
    let dir = tmp("regen");
    let path = dir.join("demo_v1.wave");
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    std::fs::copy(format!("{fixtures}/demo_v1.wave"), &path).unwrap();
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    assert!(e.open(&path).unwrap().is_empty());
    let out = e.render(0.0, 32.0, 1.0, SR).unwrap();
    let w = 960usize;
    let rms: Vec<[f64; 2]> = out
        .chunks(w * 2)
        .map(|c| {
            let n = (c.len() / 2) as f64;
            let (l, r) = c.as_chunks::<2>().0.iter().fold((0f64, 0f64), |(l, r), f| (l + (f[0] as f64).powi(2), r + (f[1] as f64).powi(2)));
            [(l / n).sqrt(), (r / n).sqrt()]
        })
        .collect();
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    let json = serde_json::json!({
        "frames": out.len() / 2,
        "note": "RMS per 20 ms window (L,R) of the demo_v1.wave fixture migrated and rendered 0..32 beats + 1 s tail at 48 kHz; notes from the engine before channels existed (commit d507fb6), regenerated for the v2 reverb and limiter DSP",
        "peak": peak,
        "rms": rms,
        "window": w,
    });
    std::fs::write(format!("{fixtures}/demo_v1_render.json"), serde_json::to_string(&json).unwrap()).unwrap();
}
