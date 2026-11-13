//! Adding, enabling or bypassing a built-in effect must never stall the command path:
//! the engine builds and prepares the new processor and hands it to the audio thread
//! in well under a frame (16 ms), even for the heavy vocal effects (Voice, Tune).

use std::sync::Arc;
use std::time::{Duration, Instant};

use wave_engine::catalog::new_fx;
use wave_engine::*;

const SR: f32 = 48000.0;
const BLOCK: usize = 256;
const KINDS: [FxKind; 12] = [
    FxKind::Eq,
    FxKind::Comp,
    FxKind::Sat,
    FxKind::Delay,
    FxKind::Reverb,
    FxKind::Limit,
    FxKind::Fet,
    FxKind::Deess,
    FxKind::Tune,
    FxKind::Denoise,
    FxKind::Voice,
    FxKind::Rider,
];
/// one 60 Hz frame
const BUDGET: Duration = Duration::from_millis(16);

/// Median of `n` runs of `f`: robust against one scheduler hiccup on a busy CI box.
fn median(n: usize, mut f: impl FnMut() -> Duration) -> Duration {
    let mut v: Vec<Duration> = (0..n).map(|_| f()).collect();
    v.sort();
    v[n / 2]
}

#[test]
fn creating_and_preparing_each_effect_is_fast() {
    for kind in KINDS {
        let t = median(5, || {
            let t0 = Instant::now();
            let mut p = wave_engine::dsp::create(kind).unwrap();
            p.prepare(SR, BLOCK);
            let f = new_fx(1, kind, 120.0);
            p.set_params(&wave_engine::catalog::param_block(&f));
            t0.elapsed()
        });
        eprintln!("create+prepare {kind:?}: {:.3} ms", t.as_secs_f64() * 1e3);
        assert!(t < BUDGET, "{kind:?} takes {t:?} to create and prepare");
    }
}

#[test]
fn adding_and_bypassing_each_effect_is_fast() {
    let mut p = Project::routed();
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    let ins = p.tracks[0].insert;
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut rt = e.create_rt(SR, BLOCK);
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let mut id = 50_000;
    for kind in KINDS {
        let add = median(5, || {
            id += 1;
            let fx = new_fx(id, kind, 120.0);
            let t0 = Instant::now();
            e.apply(&[Op::PutFx { insert_id: ins, fx, index: None }], false, None).unwrap();
            let t = t0.elapsed();
            rt.process(&mut l, &mut r, None);
            t
        });
        let mut on = true;
        let bypass = median(5, || {
            let mut fx = new_fx(id, kind, 120.0);
            fx.bypass = on;
            on = !on;
            let t0 = Instant::now();
            e.apply(&[Op::PutFx { insert_id: ins, fx, index: None }], false, None).unwrap();
            let t = t0.elapsed();
            rt.process(&mut l, &mut r, None);
            t
        });
        eprintln!("add {kind:?}: {:.3} ms, bypass toggle: {:.3} ms", add.as_secs_f64() * 1e3, bypass.as_secs_f64() * 1e3);
        assert!(add < BUDGET, "adding {kind:?} takes {add:?}");
        assert!(bypass < BUDGET, "bypassing {kind:?} takes {bypass:?}");
    }
}
