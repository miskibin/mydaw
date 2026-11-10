//! Automation of Channel Rack channels, synth and WAVE Keys parameters, and audio clips.

use std::sync::Arc;

use wave_engine::*;

const SR: u32 = 48000;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Stereo frame index of `beat` at the project's tempo (128 BPM in these projects).
fn at(beat: f64) -> usize {
    (beat * 60.0 / 128.0 * SR as f64) as usize * 2
}

fn auto_clip(id: Id, source: Id, track: u32, len: f64) -> Clip {
    Clip { id, track, start: 0.0, len, offset: 0.0, kind: ClipKind::Auto, source, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None }
}

/// Full for 2 beats, then `then`.
fn step_down(id: Id, target: AutoTarget, from: f32, then: f32) -> Automation {
    Automation { id, name: "a".into(), color: "#e0a44a".into(), target, points: vec![AutoPoint { t: 0.0, v: from }, AutoPoint { t: 1.99, v: from }, AutoPoint { t: 2.0, v: then }] }
}

/// The demo song with only the drums track audible and no master effects.
fn drums_only() -> Project {
    let mut p = Project::demo();
    p.inserts[0].fx.clear();
    for t in p.tracks.iter_mut().skip(1) {
        t.muted = true;
    }
    p
}

#[test]
fn channel_volume_automation_silences_the_channel() {
    let mut p = drums_only();
    let ids: Vec<Id> = p.channels.iter().map(|c| c.id).collect();
    for (k, id) in ids.iter().enumerate() {
        let a = 9000 + k as Id * 2;
        p.automations.push(step_down(a, AutoTarget::ChanVol { id: *id }, VOL_UNITY, 0.0));
        p.clips.push(auto_clip(a + 1, a, 7, 8.0));
    }
    let out = Engine::new(p, Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, SR).unwrap();
    assert!(rms(&out[at(0.0)..at(1.9)]) > 0.01, "silent before the automation drops");
    assert!(rms(&out[at(2.1)..at(4.0)]) < 1e-4, "channel volume automation did nothing");
}

#[test]
fn channel_pan_automation_moves_the_channel_left() {
    let mut p = drums_only();
    let ids: Vec<Id> = p.channels.iter().map(|c| c.id).collect();
    for (k, id) in ids.iter().enumerate() {
        let a = 9100 + k as Id * 2;
        p.automations.push(step_down(a, AutoTarget::ChanPan { id: *id }, 0.5, 0.0));
        p.clips.push(auto_clip(a + 1, a, 7, 8.0));
    }
    let out = Engine::new(p, Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, SR).unwrap();
    let side = |from: f64, to: f64, ch: usize| rms(&out[at(from)..at(to)].iter().skip(ch).step_by(2).copied().collect::<Vec<_>>());
    assert!(side(0.0, 1.9, 1) > 0.01, "the right side plays while centred");
    assert!(side(2.1, 4.0, 0) > 0.01, "the left side plays when panned left");
    assert!(side(2.1, 4.0, 1) < 1e-4, "pan automation left the right side playing");
}

#[test]
fn a_clips_gain_automation_glides_without_steps() {
    let dir = std::env::temp_dir().join(format!("wave-auto-clip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("tone.wav");
    // 4 s of a steady 220 Hz sine
    let tone: Vec<f32> = (0..SR as usize * 4).flat_map(|i| {
        let v = (i as f32 * 220.0 * std::f32::consts::TAU / SR as f32).sin() * 0.5;
        [v, v]
    }).collect();
    sources::write_wav(&wav, &tone, SR).unwrap();
    let mut p = Project::empty();
    p.inserts[0].fx.clear();
    let e = Engine::new(p, Arc::new(NoPlugins));
    let (_, clips) = e.import_audio(std::slice::from_ref(&wav), Some((0, 0.0))).unwrap();
    // gain ramps from +12 dB (v = 1) down to −24 dB (v = 0) over 4 beats
    let a = Automation { id: 9200, name: "g".into(), color: "#e0a44a".into(), target: AutoTarget::ClipGain { id: clips[0] }, points: vec![AutoPoint { t: 0.0, v: 1.0 }, AutoPoint { t: 4.0, v: 0.0 }] };
    e.apply(&[Op::PutAutomation { automation: a }, Op::PutClips { clips: vec![auto_clip(9201, 9200, 0, 4.0)] }], false, None).unwrap();
    let out = e.render(0.0, 4.0, 0.0, SR).unwrap();
    let early = rms(&out[at(0.1)..at(0.3)]);
    let late = rms(&out[at(3.6)..at(3.8)]);
    // ≈ 30 dB apart (the window averages the ramp)
    assert!(early / late > 20.0, "clip gain automation: early {early}, late {late}");
    // a glide, not 512-sample steps: no sample-to-sample jump beyond what the sine does itself
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    let max_step = left.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    let sine_step = 0.5 * 4.0 * 220.0 * std::f32::consts::TAU / SR as f32; // +12 dB peak slope
    assert!(max_step < sine_step * 1.05, "largest step {max_step} (a sine at +12 dB moves {sine_step})");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_muted_clip_ignores_its_automation_and_others_are_unaffected() {
    let mut p = drums_only();
    // a gain automation pointing at a pattern clip (not audio) resolves to nothing
    let pat_clip = p.clips.iter().find(|c| c.kind == ClipKind::Pat && c.track == 0).unwrap().id;
    p.automations.push(step_down(9300, AutoTarget::ClipGain { id: pat_clip }, 1.0, 0.0));
    p.clips.push(auto_clip(9301, 9300, 7, 8.0));
    let with = Engine::new(p.clone(), Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, SR).unwrap();
    p.clips.retain(|c| c.id != 9301);
    let without = Engine::new(p, Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, SR).unwrap();
    assert_eq!(with, without, "a target that does not exist must change nothing");
}

#[test]
fn synth_parameter_automation_closes_the_filter() {
    let cat = wave_engine::poly::catalog();
    let cutoff = cat.sections.iter().flat_map(|s| s.params.iter()).find(|d| d.id.contains("cutoff")).expect("a cutoff parameter").id.to_string();
    let mut p = Project::empty();
    p.inserts[0].fx.clear();
    let ch: Id = 3000;
    p.channels.push(Channel { name: "Synth".into(), src: ChannelSrc::Synth { synth: SynthState::default() }, root: 60, ..Channel::builtin(ch, Inst::Kick) });
    let notes = vec![Note { v: 0.9, ..Note::new(ch, 0.0, 64.0, Some(48)) }];
    p.patterns = vec![Pattern { id: 1, name: "P".into(), color: "#4f8ef7".into(), len: 4.0, notes, swing: 0.0 }];
    p.clips = vec![Clip { id: 1, track: 0, start: 0.0, len: 4.0, offset: 0.0, kind: ClipKind::Pat, source: 1, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None }];
    p.automations.push(step_down(9400, AutoTarget::Synth { chan_id: ch, param: cutoff }, 1.0, 0.0));
    p.clips.push(auto_clip(9401, 9400, 1, 8.0));
    let out = Engine::new(p, Arc::new(NoPlugins)).render(0.0, 4.0, 0.0, SR).unwrap();
    // brightness: energy of the first difference relative to the signal
    let bright = |a: f64, b: f64| {
        let x: Vec<f32> = out[at(a)..at(b)].iter().step_by(2).copied().collect();
        let d: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
        rms(&d) / rms(&x).max(1e-9)
    };
    let (open, closed) = (bright(1.0, 1.9), bright(2.5, 3.5));
    assert!(open > closed * 3.0, "cutoff automation: open {open}, closed {closed}");
}

#[test]
fn new_targets_round_trip_through_the_project_file() {
    let targets = vec![
        AutoTarget::ChanVol { id: 1 },
        AutoTarget::ChanPan { id: 1 },
        AutoTarget::Synth { chan_id: 2, param: "flt_cutoff".into() },
        AutoTarget::Keys { chan_id: 3, param: "release".into() },
        AutoTarget::ClipGain { id: 4 },
        AutoTarget::ClipPan { id: 4 },
    ];
    let json = serde_json::to_string(&targets).unwrap();
    assert!(json.contains(r#""kind":"chanVol""#) && json.contains(r#""chanId":2"#) && json.contains(r#""kind":"clipGain""#), "{json}");
    let back: Vec<AutoTarget> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, targets);
}

#[test]
fn an_edit_during_playback_keeps_the_automated_value() {
    // a constant tone on an audio clip, held at −24 dB by its gain automation; any edit rebuilds
    // the timeline from the clip's static 0 dB, which must not be heard for even one block
    const BLOCK: usize = 256;
    let dir = std::env::temp_dir().join(format!("wave-auto-carry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("dc.wav");
    sources::write_wav(&wav, &vec![0.25f32; SR as usize * 2 * 10], SR).unwrap();
    let mut p = Project::empty();
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    let e = Engine::new(p, Arc::new(NoPlugins));
    let (_, clips) = e.import_audio(std::slice::from_ref(&wav), Some((0, 0.0))).unwrap();
    let a = Automation { id: 9500, name: "g".into(), color: "#e0a44a".into(), target: AutoTarget::ClipGain { id: clips[0] }, points: vec![AutoPoint { t: 0.0, v: 0.0 }] };
    e.apply(&[Op::PutAutomation { automation: a }, Op::PutClips { clips: vec![auto_clip(9501, 9500, 0, 16.0)] }], false, None).unwrap();
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.play(0.0);
    let run = |rt: &mut Rt, blocks: usize| {
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        let mut peak = 0f32;
        for _ in 0..blocks {
            rt.process(&mut l, &mut r, None);
            peak = l.iter().fold(peak, |m, v| m.max(v.abs()));
        }
        peak
    };
    run(&mut rt, 40); // past the first block, which plays before any automation is applied
    let held = 0.25 * 10f32.powf(-24.0 / 20.0);
    assert!(run(&mut rt, 40) < held * 1.1, "automation holds the clip at −24 dB");
    for k in 0..5 {
        // move the automation clip's end: a new timeline each time
        e.apply(&[Op::PutClips { clips: vec![auto_clip(9501, 9500, 0, 16.0 + k as f64)] }], false, None).unwrap();
        let p = run(&mut rt, 8);
        assert!(p < held * 1.1, "edit {k}: the static 0 dB came back for a moment (peak {p})");
    }
    let _ = std::fs::remove_dir_all(dir);
}
