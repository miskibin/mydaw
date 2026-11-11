//! Live MIDI input: keys sound through the audio thread and release, the sustain pedal holds
//! them, and a take played while recording lands as a pattern clip on the armed track.

use std::sync::Arc;

use wave_engine::midi::MidiMsg;
use wave_engine::*;

const SR: u32 = 48000;
const BLOCK: usize = 256;

fn engine(p: Project) -> Arc<Engine> {
    // one directory per engine: tests run in parallel and each saves a song
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("wave-midi-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
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

/// A channel that sounds for as long as its key is held: the 808 bass sample in gate mode.
fn held_bass(p: &mut Project) -> Id {
    let id = p.factory_channel("bass-808.flac").unwrap();
    if let ChannelSrc::Sample { sampler, .. } = &mut p.channels.iter_mut().find(|c| c.id == id).unwrap().src {
        sampler.gate = true;
    }
    id
}

fn on(key: u8) -> MidiMsg {
    MidiMsg::NoteOn { ch: 0, key, vel: 100 }
}
fn off(key: u8) -> MidiMsg {
    MidiMsg::NoteOff { ch: 0, key }
}

#[test]
fn a_held_key_sounds_until_released() {
    let mut p = Project::empty();
    let bass = held_bass(&mut p);
    let e = engine(p);
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(Some(bass), true);
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
    e.midi_event(on(33));
    // still sounding a full second later: not a fixed-length audition
    assert!(run(&mut rt, SR as usize) > 0.01);
    assert!(run(&mut rt, SR as usize / 2) > 0.01);
    e.midi_event(off(33));
    run(&mut rt, SR as usize / 10); // release
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
}

#[test]
fn sustain_pedal_holds_released_keys() {
    let mut p = Project::empty();
    let bass = held_bass(&mut p);
    let e = engine(p);
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(Some(bass), true);
    e.midi_event(MidiMsg::Sustain { ch: 0, down: true });
    e.midi_event(on(33));
    e.midi_event(off(33));
    run(&mut rt, SR as usize / 10);
    assert!(run(&mut rt, SR as usize / 2) > 0.01, "pedal down: the note keeps sounding");
    e.midi_event(MidiMsg::Sustain { ch: 0, down: false });
    run(&mut rt, SR as usize / 10);
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
}

#[test]
fn panic_and_disconnect_silence_everything() {
    let mut p = Project::empty();
    let bass = held_bass(&mut p);
    let e = engine(p);
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(Some(bass), true);
    e.midi_event(on(33));
    run(&mut rt, SR as usize / 10);
    e.midi_configure(Some(bass), false); // device gone
    run(&mut rt, SR as usize / 10);
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
    // events while no device is connected are ignored
    e.midi_event(on(33));
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

#[test]
fn keys_follow_project_edits_and_stream_restarts_without_the_core_lock() {
    let mut p = Project::empty();
    p.tracks[2].arm = true;
    let e = engine(p);
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(None, true);
    // the project is edited from another thread all the time; keys must keep working
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let editor = std::thread::spawn(move || {
        let mut k = 0u32;
        while !s2.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = e2.apply(&[Op::SetBpm { bpm: 100.0 + (k % 20) as f64 }], false, None);
            k += 1;
        }
    });
    let mut worst = std::time::Duration::ZERO;
    for k in 0..200u8 {
        let t = std::time::Instant::now();
        e.midi_event(on(40 + k % 12));
        e.midi_event(off(40 + k % 12));
        worst = worst.max(t.elapsed());
        run(&mut rt, BLOCK);
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    editor.join().unwrap();
    assert!(worst < std::time::Duration::from_millis(50), "keyboard events waited {worst:?}");
    // a new stream (device change) gets the keyboard too
    let mut rt2 = e.create_rt(SR as f32, BLOCK);
    e.midi_event(on(45));
    assert!(run(&mut rt2, SR as usize / 10) > 0.01);
    e.midi_event(off(45));
}

#[test]
fn live_keys_play_where_the_song_plays_the_channel_not_through_the_armed_track() {
    // the armed track is a vocal track: its chain (compressor, pitch correction) pumped and
    // mangled chords played on the keyboard that sounded right in the piano roll
    let mut p = Project::empty();
    p.inserts[0].fx.clear();
    let id = held_bass(&mut p);
    p.patterns = vec![Pattern { id: 1, name: "P".into(), color: "#4f8ef7".into(), len: 4.0, min_len: 4.0, notes: vec![Note::new(id, 0.0, 4.0, Some(45))], swing: 0.0 }];
    p.clips = vec![Clip { id: 2, track: 0, start: 0.0, len: 4.0, offset: 0.0, kind: ClipKind::Pat, source: 1, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None }];
    p.tracks[5].arm = true;
    let vocal = p.tracks[5].insert;
    p.inserts.iter_mut().find(|m| m.id == vocal).unwrap().muted = true;
    let heard = |p: Project, key: u8| {
        let e = engine(p);
        let mut rt = e.create_rt(SR as f32, BLOCK);
        e.typing_note(id, key, 0.8, true);
        run(&mut rt, SR as usize / 4)
    };
    assert!(heard(p.clone(), 45) > 0.01, "the channel's track (the piano roll's path) is silent");
    // a channel the song does not play yet still sounds through the armed track
    let mut q = p;
    q.patterns[0].notes.clear();
    assert_eq!(heard(q, 45), 0.0);
}
