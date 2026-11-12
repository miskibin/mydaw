//! Stem export: one render per mixer insert, lined up with the mix (`Engine::stem_inserts`, `render_stem`).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use wave_engine::*;

const SR: u32 = 48_000;
/// A cancel flag nobody sets.
fn go() -> AtomicBool {
    AtomicBool::new(false)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// The demo song with a plain master (no effects, fader at 0 dB), so the stems add up to the mix.
fn plain_master() -> Project {
    let mut p = Project::sketch();
    p.inserts[0].fx.clear();
    p
}

fn engine(p: Project) -> Arc<Engine> {
    Engine::new(p, Arc::new(NoPlugins))
}

fn stem(e: &Engine, insert: Id) -> Vec<f32> {
    e.render_stem(insert, 0.0, 8.0, 0.5, SR, &go())
        .unwrap()
}

#[test]
fn the_stems_add_up_to_the_mix_and_have_the_same_length() {
    let e = engine(plain_master());
    let ids: Vec<Id> = Engine::stem_inserts(&e.project())
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        ids,
        vec![1, 2, 3],
        "Drums, Hats and Bass have clips; the other inserts are empty"
    );
    // 16 beats: the hats only come in at beat 8
    let mix = e.render(0.0, 16.0, 0.5, SR).unwrap();
    let mut sum = vec![0f32; mix.len()];
    for id in ids {
        let s = e
            .render_stem(id, 0.0, 16.0, 0.5, SR, &go())
            .unwrap();
        assert_eq!(
            s.len(),
            mix.len(),
            "insert {id}: every stem is as long as the mix"
        );
        assert!(rms(&s) > 1e-3, "insert {id} has sound");
        for (a, b) in sum.iter_mut().zip(&s) {
            *a += b;
        }
    }
    let worst = sum
        .iter()
        .zip(&mix)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert!(worst < 1e-4, "stems and mix differ by {worst}");
}

#[test]
fn each_stem_holds_only_its_own_insert() {
    let e = engine(plain_master());
    let drums = stem(&e, 1);
    let bass = stem(&e, 3);
    // the bass line starts on beat 0 like the kick; the hats (insert 2) only come in at beat 8, so
    // the first 8 beats of the hats stem are silent while the other two are not
    let hats = stem(&e, 2);
    assert!(
        rms(&hats) < 1e-5,
        "hats start at beat 8, outside this range"
    );
    assert!(rms(&drums) > 1e-3 && rms(&bass) > 1e-3);
    let both: f32 = drums.iter().zip(&bass).map(|(a, b)| (a - b).abs()).sum();
    assert!(both > 1.0, "drums and bass are different signals");
}

#[test]
fn stems_skip_the_master_chain_and_fader() {
    let loud = engine(Project::sketch()); // master limiter at its default
    let mut quiet = Project::sketch();
    quiet.inserts[0].vol = 0.3; // master fader well below 0 dB
    let quiet = engine(quiet);
    let plain = engine(plain_master());
    let a = stem(&loud, 1);
    let b = stem(&quiet, 1);
    let c = stem(&plain, 1);
    for (name, x) in [("limiter", &a), ("fader", &b)] {
        let worst = x
            .iter()
            .zip(&c)
            .map(|(p, q)| (p - q).abs())
            .fold(0f32, f32::max);
        assert!(
            worst < 1e-6,
            "the master's {name} leaked into a stem ({worst})"
        );
    }
    // while the mix does hear the master fader
    let mix = quiet.render(0.0, 8.0, 0.0, SR).unwrap();
    let ref_mix = plain.render(0.0, 8.0, 0.0, SR).unwrap();
    assert!(rms(&mix) < rms(&ref_mix) * 0.6);
}

#[test]
fn muted_and_unused_inserts_get_no_stem() {
    let mut p = Project::sketch();
    assert_eq!(
        Engine::stem_inserts(&p)
            .iter()
            .map(|x| x.1.as_str())
            .collect::<Vec<_>>(),
        ["Drums", "Hats", "Bass"]
    );
    p.inserts[2].muted = true; // Hats insert
    p.tracks[2].muted = true; // Bass track
    let names: Vec<String> = Engine::stem_inserts(&p).into_iter().map(|x| x.1).collect();
    assert_eq!(names, ["Drums"]);
    for t in p.tracks.iter_mut() {
        t.muted = true;
    }
    assert!(Engine::stem_inserts(&p).is_empty());
    // a muted track is silent in a stem too, if the insert is asked for directly
    let e = engine(p);
    assert!(rms(&stem(&e, 1)) < 1e-5);
}

#[test]
fn solo_does_not_silence_the_other_stems_but_mute_still_does() {
    let mut p = plain_master();
    p.inserts[3].solo = true; // solo on Bass
    p.tracks[0].solo = true; // and on the Drums track
    let e = engine(p);
    assert!(
        rms(&stem(&e, 1)) > 1e-3,
        "drums stem plays although another insert is soloed"
    );
    assert!(rms(&stem(&e, 3)) > 1e-3);
    // the normal export still follows solo
    let mix = e.render(0.0, 8.0, 0.0, SR).unwrap();
    let all = engine(plain_master()).render(0.0, 8.0, 0.0, SR).unwrap();
    assert!(rms(&mix) < rms(&all), "the mix honours solo");
}

#[test]
fn a_cancelled_stem_render_stops_with_the_cancel_error() {
    let e = engine(plain_master());
    let cancel = AtomicBool::new(true);
    let err = e.render_stem(1, 0.0, 8.0, 0.0, SR, &cancel).unwrap_err();
    assert!(matches!(err, EngineError::Cancelled));
    assert!(
        e.render_stem(1, 4.0, 4.0, 0.0, SR, &go()).is_err(),
        "an empty range is refused"
    );
}

#[test]
fn a_range_and_tail_apply_to_stems_like_to_the_mix() {
    let e = engine(plain_master());
    let s = e.render_stem(1, 0.0, 4.0, 1.0, SR, &go()).unwrap();
    assert_eq!(s.len(), e.render(0.0, 4.0, 1.0, SR).unwrap().len());
}
