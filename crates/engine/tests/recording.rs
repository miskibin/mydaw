//! Recording against a beat: takes line up with what the performer heard (device round
//! trip + plugin delay compensation), loop passes keep their boundaries, punch-in on the fly.

use std::path::Path;
use std::sync::Arc;

use wave_engine::catalog::new_fx;
use wave_engine::sources::Audio;
use wave_engine::*;

const SR: u32 = 48000;
const BLOCK: usize = 256;

/// No effects anywhere, track 0 armed, no count-in, recording on.
fn armed() -> Project {
    let mut p = Project::empty();
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    for t in p.tracks.iter_mut() {
        t.arm = false;
    }
    p.tracks[0].arm = true;
    p
}

/// An engine whose project is saved in its own folder, so takes land there.
fn engine(p: Project, tag: &str) -> Arc<Engine> {
    let dir = std::env::temp_dir().join(format!("wave-rec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let e = Engine::new(p, Arc::new(NoPlugins));
    e.save(&dir.join("song.wave")).unwrap();
    e
}

fn frames(beats: f64, bpm: f64) -> usize {
    (beats * 60.0 / bpm * SR as f64).round() as usize
}

/// Feeds `total` frames of input, `input(f)` for absolute input frame `f` (from `*at`).
fn feed(rt: &mut Rt, at: &mut usize, total: usize, input: &dyn Fn(usize) -> f32) {
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let end = *at + total;
    while *at < end {
        let n = BLOCK.min(end - *at);
        let inp: Vec<f32> = (0..n).map(|i| input(*at + i)).collect();
        rt.process(&mut l[..n], &mut r[..n], Some((&inp, &inp)));
        *at += n;
    }
}

fn take_audio(p: &Project, source: Id) -> Vec<f32> {
    let s = p.sources.iter().find(|s| s.id == source).expect("take source");
    let a = Audio::load(Path::new(&s.path)).expect("take file");
    a.content().chunks(2).map(|f| f[0]).collect()
}

#[test]
fn a_take_lines_up_with_the_beat_the_singer_heard() {
    // a pitch corrector on another track delays all playback (PDC); the device adds its own
    // round trip. A sound made exactly on beat 5 must land on beat 5 of the take.
    let mut p = armed();
    let other = p.tracks[1].insert;
    p.inserts.iter_mut().find(|m| m.id == other).unwrap().fx.push(new_fx(7000, FxKind::Tune, p.bpm));
    let bpm = p.bpm;
    let pdc = wave_engine::vocal::tune_latency(SR as f32);
    assert_eq!(wave_engine::engine::playback_latency(&p, SR as f32), pdc);
    let e = engine(p, "align");
    let mut rt = e.create_rt(SR as f32, BLOCK);
    let io = 1100usize;
    e.shared().set_io_latency(io as u32);
    e.set_transport(None, Some(false), Some(true), None);
    e.play(4.0);
    // the singer hears beat 5 `pdc + output` frames after the engine plays it, and the
    // mic sound reaches the engine `input` frames later: `io + pdc` after beat 5 in all
    let hit = frames(1.0, bpm) + io + pdc;
    let mut at = 0;
    feed(&mut rt, &mut at, frames(3.0, bpm), &|f| if f == hit { 0.9 } else { 0.0 });
    e.stop();
    let p = e.project();
    let clip = p.clips.iter().find(|c| c.kind == ClipKind::Audio).expect("a take");
    assert!((clip.start - 4.0).abs() < 1e-9, "take starts at {}", clip.start);
    let audio = take_audio(&p, clip.source);
    let onset = audio.iter().position(|x| x.abs() > 0.5).expect("the hit was recorded");
    assert_eq!(onset, frames(1.0, bpm), "hit lands {} frames off beat 5", onset as i64 - frames(1.0, bpm) as i64);
}

#[test]
fn loop_passes_keep_their_boundaries_with_latency() {
    let mut p = armed();
    p.loop_range = Some(LoopRange { a: 8.0, b: 12.0 });
    let bpm = p.bpm;
    let e = engine(p, "loop");
    let mut rt = e.create_rt(SR as f32, BLOCK);
    let io = 777usize;
    e.shared().set_io_latency(io as u32);
    e.set_transport(None, Some(false), Some(true), None);
    e.play(8.0);
    let pass = frames(4.0, bpm);
    // what the singer does during heard pass k reaches the engine `io` frames late
    let level = move |f: usize| if f < io { 0.0 } else { 0.1 * (((f - io) / pass) + 1) as f32 };
    let mut at = 0;
    feed(&mut rt, &mut at, pass * 3 + 2000, &level);
    e.stop();
    let p = e.project();
    let comp = &p.comps[0];
    assert_eq!(comp.takes.len(), 3, "the short tail is not a take");
    for (k, t) in comp.takes.iter().enumerate() {
        assert!((t.len - 4.0).abs() < 0.01, "take {k} is {} beats", t.len);
        let a = take_audio(&p, t.source);
        let want = 0.1 * (k + 1) as f32;
        // (the audio thread wraps on the frame at or just after the loop end, so passes
        // drift by ≤ 1 frame each against this ideal input: allow a few at the edges; a
        // misplaced boundary would be off by the whole latency, 777 frames)
        let bad = (4..a.len() - 4).find(|&i| (a[i] - want).abs() > 1e-4);
        assert_eq!(bad.map(|i| (i, a[i], a.len())), None, "take {k} holds audio of another pass (want {want})");
    }
}

#[test]
fn arming_while_playing_punches_in_at_the_playhead() {
    let p = armed();
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.play(2.0);
    let mut at = 0;
    feed(&mut rt, &mut at, BLOCK * 300, &|_| 0.3);
    let punch = e.shared().beat();
    e.set_transport(None, None, Some(true), None);
    feed(&mut rt, &mut at, SR as usize, &|_| 0.3);
    // punch out keeps the transport running and lands the take
    e.set_transport(None, None, Some(false), None);
    assert!(e.state().transport.playing);
    let p = e.project();
    let clips: Vec<_> = p.clips.iter().filter(|c| c.kind == ClipKind::Audio).collect();
    assert_eq!(clips.len(), 1, "punch-in recorded nothing");
    assert!((clips[0].start - punch).abs() < 1e-9, "take starts at {} instead of {punch}", clips[0].start);
    let secs = clips[0].len * 60.0 / p.bpm;
    assert!((secs - 1.0).abs() < 0.02, "take is {secs} s");
    e.stop();
}

#[test]
fn arming_while_playing_without_an_armed_track_records_nothing() {
    let mut p = armed();
    p.tracks[0].arm = false;
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.play(0.0);
    let mut at = 0;
    feed(&mut rt, &mut at, BLOCK * 20, &|_| 0.3);
    e.set_transport(None, None, Some(true), None);
    feed(&mut rt, &mut at, BLOCK * 50, &|_| 0.3);
    e.stop();
    assert!(e.project().clips.iter().all(|c| c.kind != ClipKind::Audio));
}
