//! Rebuilding the mixer graph (an effect added, removed or bypassed anywhere) keeps the audio
//! that is in flight in the latency-compensation lines. It used to restart every insert's PDC
//! line empty: with a long look-ahead Rider in the project, adding or switching on any effect
//! (the owner hit it with Voice) silenced the whole mix for the full compensation delay, ~1.6 s.

use std::sync::Arc;
use std::time::Instant;

use wave_engine::catalog::new_fx;
use wave_engine::*;

const SR: u32 = 48000;
const BLOCK: usize = 256;

/// Longest run of (near) digital silence, in frames.
fn longest_gap(x: &[f32]) -> usize {
    let (mut best, mut cur) = (0, 0);
    for v in x {
        if v.abs() < 1e-4 {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 0;
        }
    }
    best
}

/// A steady 15 s tone (stereo) on tracks 2, 4 and 5 (never crossing zero for long), optionally a 1.6 s Rider
/// on track 1; playing from 0 and run past the compensation delay.
fn setup(rider: bool, tag: &str) -> (Arc<Engine>, Rt) {
    let mut p = Project::routed();
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    if rider {
        let mut f = new_fx(7000, FxKind::Rider, p.bpm);
        f.params.insert("look".into(), 1.0);
        let ins = p.tracks[1].insert;
        p.inserts.iter_mut().find(|m| m.id == ins).unwrap().fx.push(f);
    }
    let e = Engine::new(p, Arc::new(NoPlugins));
    let dir = std::env::temp_dir().join(format!("wave-swap-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("tone.wav");
    let x: Vec<f32> = (0..SR as usize * 2 * 15).map(|i| 0.2 * (i as f32 * 0.05).sin() + 0.25).collect();
    wave_engine::sources::write_wav(&wav, &x, SR).unwrap();
    for t in [2, 4, 5] {
        e.import_audio(std::slice::from_ref(&wav), Some((t, 0.0))).unwrap();
    }
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.play(0.0);
    run(&mut rt, SR as usize * 2);
    (e, rt)
}

fn run(rt: &mut Rt, n: usize) -> Vec<f32> {
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n / BLOCK {
        rt.process(&mut l, &mut r, None);
        out.extend_from_slice(&l);
    }
    out
}

fn put(e: &Engine, track: usize, fx: Fx) {
    let ins = e.project().tracks[track].insert;
    e.apply(&[Op::PutFx { insert_id: ins, fx, index: None }], false, None).unwrap();
}

#[test]
fn effect_edits_do_not_drop_the_compensated_audio() {
    let (e, mut rt) = setup(true, "rider");
    assert!(longest_gap(&run(&mut rt, SR as usize / 2)) < 4, "the tone plays before any edit");
    let bpm = e.project().bpm;
    let steps: [(&str, Fx); 4] = [
        ("adding Voice", new_fx(9000, FxKind::Voice, bpm)),
        ("bypassing Voice", Fx { bypass: true, ..new_fx(9000, FxKind::Voice, bpm) }),
        ("switching Voice back on", new_fx(9000, FxKind::Voice, bpm)),
        ("adding an EQ", new_fx(9001, FxKind::Eq, bpm)),
    ];
    for (what, fx) in steps {
        put(&e, 3, fx);
        let t0 = Instant::now();
        let first = run(&mut rt, BLOCK);
        let swap_us = t0.elapsed().as_secs_f64() * 1e6;
        let mut out = first;
        out.extend(run(&mut rt, SR as usize * 3 / 2));
        let gap = longest_gap(&out);
        eprintln!("{what}: longest gap {gap} frames, block with the graph swap {swap_us:.0} us");
        assert!(gap < 4, "{what} on another insert silenced the mix for {:.0} ms", gap as f64 / SR as f64 * 1e3);
    }
}

#[test]
fn a_latency_change_costs_at_most_the_change() {
    // no Rider: Voice on track 3 raises the compensation of every other insert by its latency;
    // the audio already in the lines keeps playing, only the added delay is a pause
    let (e, mut rt) = setup(false, "plain");
    let bpm = e.project().bpm;
    put(&e, 3, new_fx(9000, FxKind::Voice, bpm));
    let lat = wave_engine::engine::playback_latency(&e.project(), SR as f32);
    assert!(lat > 0, "Voice has latency");
    let gap = longest_gap(&run(&mut rt, SR as usize));
    assert!(gap <= lat + BLOCK, "gap {gap} frames for a latency change of {lat}");
    // and bypassing it (latency back to 0) skips ahead instead of pausing
    put(&e, 3, Fx { bypass: true, ..new_fx(9000, FxKind::Voice, bpm) });
    let gap = longest_gap(&run(&mut rt, SR as usize));
    assert!(gap < 4, "gap {gap} frames when the latency drops");
}
