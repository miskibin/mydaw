//! Latency with a long look-ahead effect (the vocal Rider, ~1.6 s) in the mix: live
//! monitoring of the armed track is not delayed by it, the click and the playhead follow
//! what is heard, and takes land on the beat the singer heard.

use std::path::Path;
use std::sync::Arc;

use wave_engine::catalog::new_fx;
use wave_engine::sources::Audio;
use wave_engine::*;

const SR: u32 = 48000;
const BLOCK: usize = 256;

/// No effects anywhere, track 0 armed.
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

/// The Rider with its long look-ahead.
fn rider(id: Id, bpm: f64) -> Fx {
    let mut f = new_fx(id, FxKind::Rider, bpm);
    f.params.insert("look".into(), 1.0);
    f.params.insert("speed".into(), 0.0);
    f
}

fn add_fx(p: &mut Project, track: usize, fx: Fx) {
    let ins = p.tracks[track].insert;
    p.inserts.iter_mut().find(|m| m.id == ins).unwrap().fx.push(fx);
}

fn rider_latency(p: &Project) -> usize {
    let pdc = wave_engine::engine::playback_latency(p, SR as f32);
    assert!(pdc > SR as usize, "the long Rider should delay playback by over a second, got {pdc}");
    pdc
}

fn engine(p: Project, tag: &str) -> Arc<Engine> {
    let dir = std::env::temp_dir().join(format!("wave-lat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let e = Engine::new(p, Arc::new(NoPlugins));
    e.save(&dir.join("song.wave")).unwrap();
    e
}

/// A short burst (100 frames of 0.4) as a clip on `track` at `beat`.
fn burst(e: &Engine, track: u32, beat: f64, tag: &str) {
    let dir = std::env::temp_dir().join(format!("wave-lat-wav-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join(format!("burst{track}.wav"));
    let mut x = vec![0.4f32; 200];
    x.extend(std::iter::repeat_n(0.0, 2000));
    wave_engine::sources::write_wav(&wav, &x, SR).unwrap();
    e.import_audio(&[wav], Some((track, beat))).unwrap();
}

fn frames(beats: f64, bpm: f64) -> usize {
    (beats * 60.0 / bpm * SR as f64).round() as usize
}

/// Runs `total` frames with input `input(f)` (absolute frame `f` from `*at`); returns the left output.
fn run(rt: &mut Rt, at: &mut usize, total: usize, input: &dyn Fn(usize) -> f32) -> Vec<f32> {
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut out = Vec::with_capacity(total);
    let end = *at + total;
    while *at < end {
        let n = BLOCK.min(end - *at);
        let inp: Vec<f32> = (0..n).map(|i| input(*at + i)).collect();
        rt.process(&mut l[..n], &mut r[..n], Some((&inp, &inp)));
        out.extend_from_slice(&l[..n]);
        *at += n;
    }
    out
}

fn onset(x: &[f32], th: f32) -> Option<usize> {
    x.iter().position(|v| v.abs() > th)
}

/// Audio clips start on the first frame at or after their beat: allow that one frame.
fn assert_near(got: Option<usize>, want: usize, what: &str) {
    let got = got.unwrap_or_else(|| panic!("{what}: nothing came out"));
    assert!(got.abs_diff(want) <= 1, "{what}: at frame {got}, want {want}");
}

#[test]
fn monitored_input_is_not_delayed_by_a_rider_on_another_insert() {
    let mut p = armed();
    let bpm = p.bpm;
    add_fx(&mut p, 1, rider(7000, bpm));
    rider_latency(&p);
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.set_transport(None, None, None, Some(true));
    e.play(0.0);
    let hit = 5000;
    let mut at = 0;
    let out = run(&mut rt, &mut at, 20000, &|f| if f == hit { 0.9 } else { 0.0 });
    assert_eq!(onset(&out, 0.1), Some(hit), "the singer hears themselves late");
}

#[test]
fn monitoring_skips_look_ahead_on_its_own_insert_and_playback_stays_aligned() {
    // verse 2 recorded on the track whose insert carries the Rider (Auto-wokal on verse 1):
    // the live voice skips the look-ahead, the track's own clips still line up with the beat
    let mut p = armed();
    let bpm = p.bpm;
    add_fx(&mut p, 0, rider(7000, bpm));
    let pdc = rider_latency(&p);
    let e = Engine::new(p, Arc::new(NoPlugins));
    burst(&e, 0, 1.0, "own");
    burst(&e, 1, 1.0, "own");
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.set_transport(None, None, None, Some(true));
    e.play(0.0);
    let hit = 3000;
    let mut at = 0;
    let out = run(&mut rt, &mut at, frames(1.0, bpm) + pdc + 4000, &|f| if f == hit { 0.9 } else { 0.0 });
    assert_eq!(onset(&out, 0.1), Some(hit), "live input delayed on its own insert");
    // both clips sound together, `pdc` after beat 1 was rendered: 0.4 + 0.4
    let beat1 = frames(1.0, bpm) + pdc;
    let after = &out[hit + 10..];
    let first = onset(after, 0.1).map(|i| i + hit + 10);
    assert_eq!(first, Some(beat1), "playback is not compensated on the monitored insert");
    let peak = out[beat1..beat1 + 150].iter().fold(0f32, |a, b| a.max(b.abs()));
    assert!((peak - 0.8).abs() < 0.02, "the two clips do not line up (peak {peak})");
}

#[test]
fn the_click_lines_up_with_the_music_under_a_long_latency() {
    // metronome only, with and without a Rider on another track: the click train is the
    // same, just `pdc` later, i.e. still on the beats the music is heard on
    let clicks = |with_rider: bool, len: usize| {
        let mut p = armed();
        let bpm = p.bpm;
        if with_rider {
            add_fx(&mut p, 1, rider(7000, bpm));
        }
        let pdc = wave_engine::engine::playback_latency(&p, SR as f32);
        let e = Engine::new(p, Arc::new(NoPlugins));
        let mut rt = e.create_rt(SR as f32, BLOCK);
        e.set_transport(Some(true), None, None, None);
        e.play(0.0);
        let mut at = 0;
        (run(&mut rt, &mut at, len, &|_| 0.0), pdc)
    };
    let bpm = armed().bpm;
    let span = frames(3.0, bpm);
    let (plain, zero) = clicks(false, span);
    assert_eq!(zero, 0);
    let first = onset(&plain, 1e-4).expect("no click");
    let pdc = rider_latency(&{
        let mut p = armed();
        add_fx(&mut p, 1, rider(7000, bpm));
        p
    });
    let (late, _) = clicks(true, pdc + span);
    assert_eq!(onset(&late, 1e-4), Some(pdc + first), "the click on beat 0 must come out with the music on beat 0");
    let diff = plain.iter().zip(&late[pdc..]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(diff < 1e-6, "the delayed click train differs by {diff}");

    // and a clip on beat 2 comes out `pdc` after beat 2, where the click on beat 2 is
    let mut p = armed();
    add_fx(&mut p, 1, rider(7000, bpm));
    let e = Engine::new(p, Arc::new(NoPlugins));
    burst(&e, 0, 2.0, "click");
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.play(0.0);
    let mut at = 0;
    let out = run(&mut rt, &mut at, frames(2.0, bpm) + pdc + 5000, &|_| 0.0);
    assert_near(onset(&out, 0.3), frames(2.0, bpm) + pdc, "clip on beat 2");
    let click2 = onset(&plain[frames(1.5, bpm)..], 1e-4).map(|i| i + frames(1.5, bpm) + pdc);
    assert_near(click2, frames(2.0, bpm) + pdc + first, "click on beat 2 (with the music)");
}

fn take_audio(p: &Project, source: Id) -> Vec<f32> {
    let s = p.sources.iter().find(|s| s.id == source).expect("take source");
    let a = Audio::load(Path::new(&s.path)).expect("take file");
    a.content().chunks(2).map(|f| f[0]).collect()
}

/// Records with monitoring on and a Rider on another insert (with `!mid` also on the armed
/// one); a hit sung exactly when beat 5 is heard must land on beat 5 of the take. With
/// `mid`, the other Rider is bypassed halfway through the take (a latency change).
fn record_hit(tag: &str, mid: bool) {
    let mut p = armed();
    let bpm = p.bpm;
    if !mid {
        add_fx(&mut p, 0, rider(7000, bpm));
    }
    add_fx(&mut p, 1, rider(7001, bpm));
    let pdc = rider_latency(&p);
    let e = engine(p, tag);
    burst(&e, 2, 5.0, tag);
    let mut rt = e.create_rt(SR as f32, BLOCK);
    let io = 900usize;
    e.shared().set_io_latency(io as u32);
    e.set_transport(None, Some(false), Some(true), Some(true));
    e.play(4.0);
    let heard5 = frames(1.0, bpm) + pdc;
    let hit = heard5 + io;
    let mut at = 0;
    let mut out = run(&mut rt, &mut at, heard5 / 2, &|_| 0.0);
    if mid {
        let ins = e.project().tracks[1].insert;
        let mut off = rider(7001, bpm);
        off.bypass = true;
        e.apply(&[Op::PutFx { insert_id: ins, fx: off, index: None }], false, None).unwrap();
        assert_eq!(wave_engine::engine::playback_latency(&e.project(), SR as f32), 0);
    }
    out.extend(run(&mut rt, &mut at, hit + 20000 - heard5 / 2, &|f| if f == hit { 0.9 } else { 0.0 }));
    // the singer hears beat 5 when the take's `skip` assumes it
    assert_near(onset(&out, 0.3), heard5, "beat 5 heard when the take assumes it");
    e.stop();
    let p = e.project();
    let clip = p.clips.iter().find(|c| c.kind == ClipKind::Audio && c.track == 0).expect("a take");
    assert!((clip.start - 4.0).abs() < 1e-9, "take starts at {}", clip.start);
    let audio = take_audio(&p, clip.source);
    let on = audio.iter().position(|x| x.abs() > 0.5).expect("the hit was recorded");
    assert_eq!(on, frames(1.0, bpm), "hit lands {} frames off beat 5", on as i64 - frames(1.0, bpm) as i64);
    if mid {
        // the take is over: the deferred change applies, beat 5 now comes out undelayed
        e.set_transport(None, None, Some(false), Some(false));
        e.play(4.0);
        let out = run(&mut rt, &mut at, frames(1.0, bpm) + 5000, &|_| 0.0);
        assert_near(onset(&out, 0.3), frames(1.0, bpm), "beat 5 undelayed once the take is over");
    }
}

#[test]
fn a_take_lines_up_with_the_beat_with_riders_and_monitoring() {
    record_hit("rider", false);
}

#[test]
fn a_latency_change_during_a_take_waits_until_it_ends() {
    record_hit("mid", true);
}

#[test]
fn the_playhead_shows_the_heard_beat() {
    let mut p = armed();
    let bpm = p.bpm;
    add_fx(&mut p, 1, rider(7000, bpm));
    let pdc = rider_latency(&p);
    let bps = bpm / 60.0 / SR as f64;
    let e = Engine::new(p, Arc::new(NoPlugins));
    burst(&e, 2, 16.0, "head"); // a song long enough to start on beat 4
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.play(4.0);
    let mut at = 0;
    // before the first sound has come out, the playhead waits at the start
    run(&mut rt, &mut at, BLOCK * 20, &|_| 0.0);
    assert_eq!(e.shared().heard_beat(), 4.0);
    assert!(e.shared().beat() > 4.0);
    let total = BLOCK * ((pdc + 30000) / BLOCK);
    run(&mut rt, &mut at, total - BLOCK * 20, &|_| 0.0);
    let s = e.shared();
    let want = s.beat() - pdc as f64 * bps;
    assert!((s.heard_beat() - want).abs() < 1e-9, "heard {} want {want}", s.heard_beat());
    assert!((s.heard_beat() - (4.0 + (total - pdc) as f64 * bps)).abs() < 1e-6);
    // the telemetry frame carries the heard beat
    let frame = wave_engine::telemetry::frame(1, &s, &[], None, None);
    let beat = f64::from_le_bytes(frame[8..16].try_into().unwrap());
    assert_eq!(beat, s.heard_beat());
}

#[test]
fn the_playhead_wraps_with_the_loop_under_latency() {
    let mut p = armed();
    let bpm = p.bpm;
    add_fx(&mut p, 1, rider(7000, bpm));
    p.loop_range = Some(LoopRange { a: 0.0, b: 8.0 });
    let pdc = rider_latency(&p);
    let bps = bpm / 60.0 / SR as f64;
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.play(0.0);
    let mut at = 0;
    // just past the loop end: the renderer is at the start again, the ear still near the end
    run(&mut rt, &mut at, frames(8.0, bpm) + 2 * BLOCK, &|_| 0.0);
    let s = e.shared();
    assert!(s.beat() < 1.0, "loop did not wrap: {}", s.beat());
    let heard = s.heard_beat();
    let want = 8.0 + s.beat() - pdc as f64 * bps;
    assert!((heard - want).abs() < 1e-6 && heard < 8.0 && heard > 4.0, "heard {heard}, want {want}");
}
