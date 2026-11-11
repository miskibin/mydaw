//! WAVE Keys: the sampled piano as a Channel Rack channel (model, ops, rendering).

use std::sync::Arc;

use wave_engine::keys::KeysBank;
use wave_engine::sources::Audio;
use wave_engine::*;

const SR: u32 = 48000;

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

/// Two seconds of a steady sine at `hz`, peak `amp` (a stand-in for one piano recording).
fn tone(hz: f64, amp: f32) -> Arc<Audio> {
    let n = SR as usize * 2;
    let mut d = Vec::with_capacity(n * 2);
    for i in 0..n {
        let v = (i as f64 * hz * std::f64::consts::TAU / SR as f64).sin() as f32 * amp;
        d.extend([v, v]);
    }
    Arc::new(Audio::from_stereo(d, SR, 2))
}

/// C4 and D#4, each soft (quiet) and loud.
fn bank() -> Arc<KeysBank> {
    Arc::new(KeysBank::from_layers(vec![(60, vec![tone(261.63, 0.1), tone(261.63, 0.5)]), (63, vec![tone(311.13, 0.1), tone(311.13, 0.5)])]).unwrap())
}

const CH: Id = 2000;

fn project(notes: Vec<Note>, keys: KeysState) -> Project {
    let mut p = Project::empty();
    p.inserts[0].fx.clear();
    p.channels.push(Channel { name: "WAVE Keys".into(), src: ChannelSrc::Keys { keys }, root: 60, ..Channel::builtin(CH, Inst::Kick) });
    p.patterns = vec![Pattern { id: 1, name: "P".into(), color: "#4f8ef7".into(), len: 4.0, notes, swing: 0.0 }];
    p.clips = vec![Clip { id: 1, track: 0, start: 0.0, len: 4.0, offset: 0.0, kind: ClipKind::Pat, source: 1, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None }];
    p
}

fn render(p: Project, with_bank: bool) -> Vec<f32> {
    let e = Engine::new(p, Arc::new(NoPlugins));
    if with_bank {
        e.set_keys_bank(Some(bank()));
    }
    e.render(0.0, 4.0, 0.0, SR).unwrap().chunks_exact(2).map(|f| f[0]).collect()
}

fn note(step: f32, len: f32, key: u8, v: f32) -> Note {
    Note { v, ..Note::new(CH, step, len, Some(key)) }
}

/// Dominant frequency of `x` by zero crossings.
fn freq(x: &[f32]) -> f64 {
    let n = x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    n as f64 * SR as f64 / x.len() as f64
}

#[test]
fn silent_until_the_piano_is_loaded() {
    let p = project(vec![note(0.0, 4.0, 60, 0.9)], KeysState::default());
    assert_eq!(rms(&render(p.clone(), false)), 0.0);
    assert!(rms(&render(p, true)) > 0.05);
}

#[test]
fn plays_the_nearest_recording_at_the_note_pitch() {
    // 128 BPM: a sixteenth is 5625 frames. D4 (62) is nearer D#4 (63): that recording, one semitone down.
    let x = render(project(vec![note(0.0, 8.0, 62, 0.9)], KeysState::default()), true);
    let f = freq(&x[2000..40000]);
    assert!((f - 293.66).abs() < 3.0, "D4 expected, got {f:.1} Hz");
    let x = render(project(vec![note(0.0, 8.0, 60, 0.9)], KeysState::default()), true);
    let f = freq(&x[2000..40000]);
    assert!((f - 261.63).abs() < 3.0, "C4 expected, got {f:.1} Hz");
}

#[test]
fn velocity_picks_the_layer_and_dynamics_scales_inside_it() {
    let level = |v: f32, dynamics: f32| rms(&render(project(vec![note(0.0, 8.0, 60, v)], KeysState { dynamics, ..Default::default() }), true)[2000..40000]);
    // the loud recording is five times the soft one
    assert!(level(0.9, 0.0) / level(0.2, 0.0) > 4.5);
    // with dynamics off every note of a layer is as loud as the recording
    assert!((level(0.6, 0.0) / level(0.9, 0.0) - 1.0).abs() < 0.01);
    // with dynamics on a harder hit inside the layer is louder
    assert!(level(0.95, 100.0) > level(0.6, 100.0) * 1.3);
}

#[test]
fn a_note_rings_for_its_length_then_fades_over_the_release() {
    // a one-step note (5625 frames), then silence
    let short = render(project(vec![note(0.0, 1.0, 60, 0.9)], KeysState { release: 40.0, ..Default::default() }), true);
    let long = render(project(vec![note(0.0, 1.0, 60, 0.9)], KeysState { release: 2000.0, ..Default::default() }), true);
    assert!(rms(&short[1000..5000]) > 0.05, "the note sounds while held");
    // 100 ms after the key is let go: gone with a 40 ms release, still ringing with 2 s
    let after = 5625 + 4800..5625 + 9600;
    assert!(rms(&short[after.clone()]) < 1e-3);
    assert!(rms(&long[after]) > 0.025);
    // and a release never ends in a step (no click): the largest jump stays small
    let jump = short.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    assert!(jump < 0.05, "click of {jump}");
}

#[test]
fn settings_are_validated_saved_and_undone() {
    let mut p = project(vec![], KeysState::default());
    let ch = p.channels.iter().find(|c| c.id == CH).unwrap().clone();
    let bad = Channel { src: ChannelSrc::Keys { keys: KeysState { release: 5.0, dynamics: 100.0 } }, ..ch.clone() };
    assert!(ops::apply(&mut p, &Op::PutChannel { channel: bad, index: None }).is_err());
    let bad = Channel { src: ChannelSrc::Keys { keys: KeysState { release: 300.0, dynamics: 140.0 } }, ..ch.clone() };
    assert!(ops::apply(&mut p, &Op::PutChannel { channel: bad, index: None }).is_err());

    let e = Engine::new(p, Arc::new(NoPlugins));
    let good = Channel { src: ChannelSrc::Keys { keys: KeysState { release: 900.0, dynamics: 40.0 } }, ..ch };
    e.apply(&[Op::PutChannel { channel: good.clone(), index: None }], false, None).unwrap();
    assert_eq!(e.project().channel(CH).unwrap().src, good.src);
    assert!(e.project().channel(CH).unwrap().pitched());

    // JSON: {"kind":"keys","keys":{"release":900,"dynamics":40}}, and a bare {"kind":"keys"} loads with the defaults
    let v = serde_json::to_value(e.project().channel(CH).unwrap()).unwrap();
    assert_eq!(v["kind"], "keys");
    assert_eq!(v["keys"]["release"], 900.0);
    let mut bare = v.clone();
    bare.as_object_mut().unwrap().remove("keys");
    let c: Channel = serde_json::from_value(bare).unwrap();
    assert_eq!(c.src, ChannelSrc::Keys { keys: KeysState::default() });

    e.undo();
    assert_eq!(e.project().channel(CH).unwrap().src, ChannelSrc::Keys { keys: KeysState::default() });
}

#[test]
fn a_chord_is_the_plain_sum_of_its_notes() {
    // a limiter on the piano used to squash chords (pumping, the tone changing as keys were
    // added): every note must sound in the chord exactly as it sounds alone
    let keys: Vec<u8> = (58..66).collect();
    let chord = render(project(keys.iter().map(|&k| note(0.0, 8.0, k, 1.0)).collect(), KeysState::default()), true);
    let mut sum = vec![0f32; chord.len()];
    for &k in &keys {
        for (s, x) in sum.iter_mut().zip(render(project(vec![note(0.0, 8.0, k, 1.0)], KeysState::default()), true)) {
            *s += x;
        }
    }
    let err = chord.iter().zip(&sum).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(err < 1e-4, "chord differs from the sum of its notes by {err}");
}

#[test]
fn a_loud_note_leaves_headroom_for_chords() {
    let x = render(project(vec![note(0.0, 8.0, 60, 1.0)], KeysState::default()), true);
    let pk = x.iter().fold(0f32, |a, b| a.max(b.abs()));
    // the recording peaks at 0.5 (−6 dBFS, about −3 dBFS after the velocity curve); the
    // channel plays it 12 dB lower
    let db = 20.0 * pk.log10();
    assert!(db < -14.0 && db > -16.0, "loud note peak {db:.1} dBFS");
}
