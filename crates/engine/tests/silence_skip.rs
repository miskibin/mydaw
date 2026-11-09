//! Silence skipping of effect chains must not be audible: renders with skipping on and off
//! agree to better than -120 dB, also across a long gap that parks every chain.

use std::sync::Arc;

use wave_engine::catalog::new_fx;
use wave_engine::*;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// A demo project whose inserts carry the built-in effects that have audible tails.
fn tail_heavy() -> Project {
    let mut p = Project::demo();
    let bpm = p.bpm;
    let mut next = 9000;
    for (k, ins) in p.inserts.iter_mut().enumerate().skip(1) {
        if k <= 2 {
            // the pitch/voice/noise effects (and the rider) idle after a second of silence by
            // themselves. The rider rides wherever it sits in a vocal chain: here it sees the
            // clips directly, so its gain does not scale the float-level residue differences
            // the tails further down leave (-130 dB; amplified, they would exceed the limit)
            for kind in [FxKind::Denoise, FxKind::Tune, FxKind::Voice, FxKind::Rider] {
                next += 1;
                let mut fx = new_fx(next, kind, bpm);
                if kind == FxKind::Rider {
                    // short look-ahead: the wake-up test listens for sound within ~1 s
                    fx.params.insert("look".into(), 0.0);
                }
                ins.fx.push(fx);
            }
        }
        for kind in [FxKind::Eq, FxKind::Comp, FxKind::Sat, FxKind::Delay, FxKind::Reverb, FxKind::Deess, FxKind::Fet, FxKind::Limit] {
            next += 1;
            let mut fx = new_fx(next, kind, bpm);
            if kind == FxKind::Reverb || kind == FxKind::Delay {
                fx.mix = 0.5;
            }
            ins.fx.push(fx);
        }
    }
    p
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()))
}

#[test]
fn skipping_matches_full_processing() {
    let e = Engine::new(tail_heavy(), Arc::new(NoPlugins));
    // 8 beats of music, then 40 s of ringing and silence: tails, parking, long silence
    let on = e.render(0.0, 8.0, 40.0, 48000).unwrap();
    e.set_silence_skip(false);
    let off = e.render(0.0, 8.0, 40.0, 48000).unwrap();
    let d = max_diff(&on, &off);
    assert!(rms(&off[..96000]) > 0.01, "the project must make sound");
    assert!(d < 1e-6, "skipping changed the render by {d} (> -120 dB)");
}

#[test]
fn skipping_wakes_up_on_new_signal() {
    // music, a long gap that parks every chain, then music again: the second burst must match
    let mut p = tail_heavy();
    let shift = 200.0; // beats
    for c in p.clips.clone() {
        let mut c = c;
        c.id += 100_000;
        c.start += shift;
        p.clips.push(c);
    }
    let e = Engine::new(p, Arc::new(NoPlugins));
    let to = shift + 8.0;
    let on = e.render(0.0, to, 2.0, 48000).unwrap();
    e.set_silence_skip(false);
    let off = e.render(0.0, to, 2.0, 48000).unwrap();
    let d = max_diff(&on, &off);
    assert!(d < 1e-6, "skipping changed the render by {d}");
    assert!(rms(&off[off.len() - 96000..]) > 1e-4, "the second burst must sound");
}

#[test]
fn silent_inserts_are_parked_and_wake_up_live() {
    let e = Engine::new(tail_heavy(), Arc::new(NoPlugins));
    let mut rt = e.create_rt(48000.0, 512);
    let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
    // nothing plays: after the longest tail every insert (master included) is parked
    for _ in 0..(48000 * 60 / 512) {
        rt.process(&mut l, &mut r, None);
    }
    assert_eq!(rt.parked_inserts(), e.project().inserts.len());
    e.play(0.0);
    let mut energy = 0f32;
    for _ in 0..100 {
        rt.process(&mut l, &mut r, None);
        energy += rms(&l);
    }
    assert!(energy > 0.5, "nothing playing after waking up ({energy})");
    assert!(rt.parked_inserts() < e.project().inserts.len());
}

#[test]
fn parallel_render_is_bit_identical() {
    let e = Engine::new(tail_heavy(), Arc::new(NoPlugins));
    e.set_render_workers(1);
    let one = e.render(0.0, 8.0, 3.0, 48000).unwrap();
    e.set_render_workers(6);
    let six = e.render(0.0, 8.0, 3.0, 48000).unwrap();
    assert_eq!(one.len(), six.len());
    assert!(one.iter().zip(&six).all(|(a, b)| a.to_bits() == b.to_bits()), "worker threads changed the render");
    assert!(rms(&one) > 0.01);
}
