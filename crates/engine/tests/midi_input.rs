//! Live MIDI input: keys sound through the audio thread and release, the sustain pedal holds
//! them, and a take played while recording lands as a pattern clip on the armed track.

use std::sync::Arc;

use wave_engine::midi::MidiMsg;
use wave_engine::*;

const SR: u32 = 48000;
const BLOCK: usize = 256;

fn engine(p: Project) -> Arc<Engine> {
    let dir = std::env::temp_dir().join(format!("wave-midi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let e = Engine::new(p, Arc::new(NoPlugins));
    e.save(&dir.join("song.wave")).unwrap();
    e
}

/// Renders `frames` of output and returns the peak of the master bus.
fn run(rt: &mut Rt, frames: usize) -> f32 {
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut peak = 0f32;
    let mut done = 0;
    while done < frames {
        let n = BLOCK.min(frames - done);
        rt.process(&mut l[..n], &mut r[..n], None);
        peak = l[..n].iter().chain(r[..n].iter()).fold(peak, |a, x| a.max(x.abs()));
        done += n;
    }
    peak
}

fn on(key: u8) -> MidiMsg {
    MidiMsg::NoteOn { ch: 0, key, vel: 100 }
}
fn off(key: u8) -> MidiMsg {
    MidiMsg::NoteOff { ch: 0, key }
}

#[test]
fn a_held_key_sounds_until_released() {
    let e = engine(Project::empty());
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(None, true);
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
    e.midi_event(on(45));
    // still sounding a full second later: not a fixed-length audition
    assert!(run(&mut rt, SR as usize) > 0.01);
    assert!(run(&mut rt, SR as usize / 2) > 0.01);
    e.midi_event(off(45));
    run(&mut rt, SR as usize / 10); // release
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
}

#[test]
fn sustain_pedal_holds_released_keys() {
    let e = engine(Project::empty());
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(None, true);
    e.midi_event(MidiMsg::Sustain { ch: 0, down: true });
    e.midi_event(on(40));
    e.midi_event(off(40));
    run(&mut rt, SR as usize / 10);
    assert!(run(&mut rt, SR as usize / 2) > 0.01, "pedal down: the note keeps sounding");
    e.midi_event(MidiMsg::Sustain { ch: 0, down: false });
    run(&mut rt, SR as usize / 10);
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
}

#[test]
fn panic_and_disconnect_silence_everything() {
    let e = engine(Project::empty());
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(None, true);
    e.midi_event(on(50));
    run(&mut rt, SR as usize / 10);
    e.midi_configure(None, false); // device gone
    run(&mut rt, SR as usize / 10);
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
    // events while no device is connected are ignored
    e.midi_event(on(50));
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
}

#[test]
fn a_recorded_take_becomes_a_pattern_clip_on_the_armed_track() {
    let mut p = Project::empty();
    p.tracks[2].arm = true;
    let bpm = p.bpm;
    let e = engine(p);
    let mut rt = e.create_rt(SR as f32, BLOCK);
    let beat = |b: f64| (b * 60.0 / bpm * SR as f64) as usize;
    e.midi_configure(None, true);
    e.set_transport(None, Some(false), Some(true), None);
    e.play(4.0);
    run(&mut rt, beat(0.5)); // playhead at 4.5
    e.midi_event(on(48));
    run(&mut rt, beat(0.5)); // 5.0
    e.midi_event(off(48));
    run(&mut rt, beat(1.0)); // 6.0
    e.midi_event(on(52)); // still held at stop
    run(&mut rt, beat(0.5)); // 6.5
    e.stop();
    let p = e.project();
    let clip = p.clips.iter().find(|c| c.kind == ClipKind::Pat).expect("a pattern clip");
    assert_eq!(clip.track, 2);
    assert_eq!(clip.start, 4.0);
    assert_eq!(clip.len, 4.0);
    let pat = p.patterns.iter().find(|x| x.id == clip.source).unwrap();
    assert_eq!(pat.len, 4.0);
    assert_eq!(pat.notes.len(), 2);
    let a = &pat.notes[0];
    assert_eq!(a.p, Some(48));
    assert!((a.step - 2.0).abs() < 0.2, "starts at step {}", a.step); // 0.5 beat in = step 2
    assert!((a.len - 2.0).abs() < 0.2, "{} steps long", a.len);
    let b = &pat.notes[1];
    assert_eq!(b.p, Some(52));
    assert!((b.step - 8.0).abs() < 0.2, "starts at step {}", b.step);
    assert!(b.len > 1.0, "the held note ends at the stop");
    // the take is one undo step
    assert!(e.undo());
    assert!(e.project().clips.iter().all(|c| c.kind != ClipKind::Pat));
}

#[test]
fn nothing_played_adds_nothing() {
    let mut p = Project::empty();
    p.tracks[0].arm = true;
    let e = engine(p);
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(None, true);
    e.set_transport(None, Some(false), Some(true), None);
    e.play(0.0);
    run(&mut rt, SR as usize / 2);
    e.stop();
    assert!(e.project().clips.is_empty());
}

#[test]
fn not_armed_or_not_recording_plays_but_does_not_record() {
    let mut p = Project::empty();
    p.tracks[0].arm = true;
    let e = engine(p);
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(None, true);
    e.play(0.0); // rec off
    run(&mut rt, 1000);
    e.midi_event(on(45));
    assert!(run(&mut rt, 4000) > 0.0);
    e.stop();
    assert!(e.project().clips.is_empty());
}
