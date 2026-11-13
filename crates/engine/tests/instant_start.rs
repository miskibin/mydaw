//! Instant start under plugin delay compensation: with a long look-ahead Rider in the mix
//! (≈ 1.6 s of latency), the music at the start position is heard right after Play, a seek
//! or a loop restart — not after the latency — and still on the timeline, without clicks.

use std::sync::Arc;
use std::time::Instant;

use wave_engine::catalog::new_fx;
use wave_engine::*;

const SR: u32 = 48000;
const BLOCK: usize = 256;

fn project() -> Project {
    let mut p = Project::routed();
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    for t in p.tracks.iter_mut() {
        t.arm = false;
    }
    // the long Rider on track 1's insert: every other insert is compensated to it
    let mut f = new_fx(7000, FxKind::Rider, p.bpm);
    f.params.insert("look".into(), 1.0);
    let ins = p.tracks[1].insert;
    p.inserts.iter_mut().find(|m| m.id == ins).unwrap().fx.push(f);
    p
}

fn frames(beats: f64, bpm: f64) -> usize {
    (beats * 60.0 / bpm * SR as f64).round() as usize
}

fn engine(tag: &str) -> (Arc<Engine>, usize) {
    let p = project();
    let pdc = wave_engine::engine::playback_latency(&p, SR as f32);
    assert!(pdc > SR as usize, "the long Rider should delay playback by over a second, got {pdc}");
    let dir = std::env::temp_dir().join(format!("wave-instant-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let e = Engine::new(p, Arc::new(NoPlugins));
    e.save(&dir.join("song.wave")).unwrap();
    (e, pdc)
}

/// A mono signal (written as stereo) as a clip on `track` at `beat`.
fn clip(e: &Engine, track: u32, beat: f64, x: &[f32], tag: &str) {
    let dir = std::env::temp_dir().join(format!("wave-instant-wav-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join(format!("clip{track}_{}.wav", (beat * 1000.0) as u64));
    let st: Vec<f32> = x.iter().flat_map(|v| [*v, *v]).collect();
    wave_engine::sources::write_wav(&wav, &st, SR).unwrap();
    e.import_audio(&[wav], Some((track, beat))).unwrap();
}

/// 100 frames of 0.4, then silence.
fn burst() -> Vec<f32> {
    let mut x = vec![0.4f32; 100];
    x.extend(std::iter::repeat_n(0.0, 2000));
    x
}

fn run(rt: &mut Rt, frames: usize) -> Vec<f32> {
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut out = Vec::with_capacity(frames);
    while out.len() < frames {
        let n = BLOCK.min(frames - out.len());
        rt.process(&mut l[..n], &mut r[..n], None);
        out.extend_from_slice(&l[..n]);
    }
    out
}

fn onset(x: &[f32], th: f32) -> Option<usize> {
    x.iter().position(|v| v.abs() > th)
}

#[test]
fn play_is_heard_at_once_and_on_the_timeline() {
    let (e, pdc) = engine("play");
    let bpm = e.project().bpm;
    // on the uncompensated track and on the Rider's own track
    clip(&e, 0, 5.0, &burst(), "play");
    clip(&e, 1, 6.0, &burst(), "play");
    let mut rt = e.create_rt(SR as f32, BLOCK);
    rt.set_instant_start(Some(f32::INFINITY));
    e.play(4.0);
    let out = run(&mut rt, frames(3.0, bpm));
    assert_eq!(rt.preroll(), 0);
    // beat 5 is one beat after the start, beat 6 two: no latency in between
    let a = onset(&out, 0.3).expect("beat 5 not heard");
    assert!(a.abs_diff(frames(1.0, bpm)) <= 1, "beat 5 heard at frame {a}, want {} (pdc {pdc})", frames(1.0, bpm));
    let b = onset(&out[a + 200..], 0.3).map(|i| i + a + 200).expect("beat 6 not heard");
    assert!(b.abs_diff(frames(2.0, bpm)) <= 1, "beat 6 (Rider track) heard at frame {b}, want {}", frames(2.0, bpm));
    // the playhead shows what is heard
    let bps = bpm / 60.0 / SR as f64;
    assert!((e.shared().heard_beat() - (4.0 + out.len() as f64 * bps)).abs() < 2.0 * bps, "heard beat {}", e.shared().heard_beat());
}

#[test]
fn seek_and_loop_restart_are_heard_at_once() {
    let (e, _) = engine("seek");
    let bpm = e.project().bpm;
    clip(&e, 0, 8.0, &burst(), "seek");
    clip(&e, 0, 0.0, &burst(), "seek");
    let mut rt = e.create_rt(SR as f32, BLOCK);
    rt.set_instant_start(Some(f32::INFINITY));
    e.play(0.0);
    let first = run(&mut rt, BLOCK * 40);
    assert_eq!(onset(&first, 0.3), Some(0), "the clip on the start beat");
    // seek: the clip on beat 8 is heard right after the jump
    e.play(8.0);
    let out = run(&mut rt, BLOCK * 8);
    assert_eq!(onset(&out, 0.3), Some(0), "after a seek");

    // loop 0..2 from beat 1: the clip on beat 0 comes back exactly when the loop wraps
    e.apply(&[Op::SetLoop { range: Some(LoopRange { a: 0.0, b: 2.0 }) }], false, None).unwrap();
    e.play(1.0);
    let out = run(&mut rt, frames(1.0, bpm) + 4000);
    let w = onset(&out, 0.3).expect("the loop start not heard after the wrap");
    assert!(w.abs_diff(frames(1.0, bpm)) <= 1, "loop start heard at {w}, want {}", frames(1.0, bpm));
}

#[test]
fn a_jump_while_sound_plays_does_not_click() {
    let (e, _) = engine("click");
    // a 0.5 amplitude 110 Hz sine over 16 beats: a seek lands in another phase
    let x: Vec<f32> = (0..frames(16.0, 120.0)).map(|i| 0.5 * (i as f32 * 110.0 * std::f32::consts::TAU / SR as f32).sin()).collect();
    clip(&e, 0, 0.0, &x, "click");
    let mut rt = e.create_rt(SR as f32, BLOCK);
    rt.set_instant_start(Some(f32::INFINITY));
    e.play(0.0);
    let mut out = run(&mut rt, BLOCK * 37 + 100);
    for to in [5.13, 2.71, 9.0] {
        e.play(to);
        out.extend(run(&mut rt, BLOCK * 20 + 77));
    }
    // the steepest step of the sine itself, with a margin
    let slope = 0.5 * 110.0 * std::f32::consts::TAU / SR as f32;
    let step = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    assert!(step < slope * 1.6, "a jump steps by {step}, the sine by at most {slope}");
    assert!(out[out.len() - 2000..].iter().any(|v| v.abs() > 0.4), "music after the last jump");
}

#[test]
fn a_bounded_pre_roll_is_heard_soon_and_on_the_timeline() {
    // a live budget: the pre-roll takes a few callbacks at most on this light mix
    let (e, pdc) = engine("budget");
    let bpm = e.project().bpm;
    clip(&e, 0, 4.0, &burst(), "budget");
    clip(&e, 0, 5.0, &burst(), "budget");
    // a quiet voice-like tone through the Rider, so its chain really works during the pre-roll
    let v: Vec<f32> = (0..frames(12.0, bpm)).map(|i| 0.05 * (i as f32 * 220.0 * std::f32::consts::TAU / SR as f32).sin()).collect();
    clip(&e, 1, 0.0, &v, "budget");
    let mut rt = e.create_rt(SR as f32, BLOCK);
    rt.set_instant_start(Some(0.7));
    // warm the caches as a running stream would be
    run(&mut rt, BLOCK * 8);
    let t = Instant::now();
    e.play(4.0);
    let out = run(&mut rt, pdc + frames(2.0, bpm));
    let wall = t.elapsed().as_secs_f64();
    let first = onset(&out, 0.3).expect("nothing heard");
    let ms = first as f64 * 1000.0 / SR as f64;
    let render_ms = wall * 1000.0 * pdc as f64 / out.len() as f64;
    println!(
        "pdc {:.0} ms · first sound after {ms:.1} ms of device time (pre-roll at {render_ms:.1} ms of CPU)",
        pdc as f64 * 1000.0 / SR as f64
    );
    // whatever the speed, the music is on the timeline: the next clip one beat later
    let next = onset(&out[first + 200..], 0.3).map(|i| i + first + 200).expect("beat 5 not heard");
    assert!(next.abs_diff(first + frames(1.0, bpm)) <= 1, "beat 5 at {next}, beat 4 at {first}");
    assert!(first < pdc, "no faster than without pre-roll ({first} frames)");
    if !cfg!(debug_assertions) {
        assert!(ms < 60.0, "first sound after {ms:.1} ms");
    }
}
