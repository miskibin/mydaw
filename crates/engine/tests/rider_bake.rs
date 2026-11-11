//! Baking the Vocal Rider into a clip's audio (`Engine::bake_rider`): what the clip then
//! plays equals the live rider on its insert with plugin delay compensation, undo restores
//! the original source, and the auto-vocal plan with a baked rider renders like the plan
//! with the rider live.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use wave_engine::autovocal::Style;
use wave_engine::catalog::new_fx;
use wave_engine::*;

const SR: u32 = 48000;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wave-bake-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// No effects anywhere, so the only processing is what the test puts on the vocal insert.
fn dry_project() -> Project {
    let mut p = Project::empty();
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    p
}

/// A sung-like take: harmonic syllables whose level wanders by word and phrase, gaps,
/// "s" bursts and a low noise bed (mono, as interleaved stereo).
fn take(secs: f64) -> Vec<f32> {
    let n = (secs * SR as f64) as usize;
    let sr = SR as f32;
    let mut s: u32 = 7;
    let mut nz = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s as f32 / u32::MAX as f32 - 0.5) * 2.0
    };
    let mut x = vec![0f32; n];
    let (mut ph, mut t0, mut k) = (0f32, (0.3 * sr) as usize, 0usize);
    while t0 < n {
        let len = ((0.25 + 0.15 * ((k * 7 % 5) as f32)) * sr) as usize;
        let lvl = -14.0 - 8.0 * ((k as f32 * 0.9).sin() + 1.0) * 0.5 - if (k / 5) % 2 == 1 { 6.0 } else { 0.0 };
        let a = 10f32.powf(lvl / 20.0);
        let f0 = 196.0 * 2f32.powf(((k * 5 % 7) as f32) / 12.0);
        for i in 0..len.min(n - t0) {
            let t = i as f32 / len as f32;
            let env = (t / 0.08).min(1.0) * ((1.0 - t) / 0.15).min(1.0);
            ph += 2.0 * std::f32::consts::PI * f0 / sr;
            let v = ph.sin() + 0.6 * (2.0 * ph).sin() + 0.3 * (3.0 * ph).sin();
            x[t0 + i] += 0.5 * a * env * v;
            if k % 3 == 0 && i < (0.08 * sr) as usize {
                x[t0 + i] += 0.3 * a * nz();
            }
        }
        t0 += len + (0.07 * sr) as usize + if k % 5 == 4 { (0.4 * sr) as usize } else { 0 };
        k += 1;
    }
    x.iter().map(|v| v + 3e-4 * nz()).flat_map(|v| [v, v]).collect()
}

/// An engine with the take imported as a clip on track 0 at beat 0; returns (engine, clip id, source id).
fn vocal_engine(tag: &str, secs: f64) -> (Arc<Engine>, Id, Id, PathBuf) {
    let dir = tmp(tag);
    let e = Engine::new(dry_project(), Arc::new(NoPlugins));
    e.save(&dir.join("song.wave")).unwrap();
    let wav = dir.join("vocal.wav");
    wave_engine::sources::write_wav(&wav, &take(secs), SR).unwrap();
    let (srcs, _) = e.import_audio(&[wav], Some((0, 0.0))).unwrap();
    let p = e.project();
    let clip = p.clips.iter().find(|c| c.source == srcs[0]).unwrap().id;
    (e, clip, srcs[0], dir)
}

fn insert_of(p: &Project, track: usize) -> Id {
    p.tracks[track].insert
}

fn rider_params(look: f64) -> BTreeMap<String, f64> {
    let mut m = wave_engine::catalog::default_params(FxKind::Rider);
    for (k, v) in [("auto", 0.0), ("target", -24.0), ("floor", -70.0), ("gate", -50.0), ("look", look), ("out", -2.5), ("up", 6.0), ("down", 6.0)] {
        m.insert(k.into(), v);
    }
    m
}

fn worst(a: &[f32], b: &[f32]) -> (usize, f32) {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).enumerate().fold((0, 0f32), |m, (i, d)| if d > m.1 { (i, d) } else { m })
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn baked_clip_plays_what_the_live_rider_did_and_undo_restores_it() {
    let (e, clip, src, dir) = vocal_engine("live", 9.0);
    let bpm = e.project().bpm;
    let spb = 60.0 / bpm;
    // a region that starts inside the source (on a whole frame) with clip gain
    let mut c = e.project().clips.iter().find(|c| c.id == clip).unwrap().clone();
    c.offset = 0.5 / spb; // 0.5 s in
    c.len = 7.5 / spb;
    c.gain = -3.0;
    let ins = insert_of(&e.project(), 0);
    for look in [1.0, 0.0] {
        let params = rider_params(look);
        let mut fx = new_fx(9000, FxKind::Rider, bpm);
        fx.params = params.clone();
        e.apply(&[Op::PutClips { clips: vec![c.clone()] }, Op::PutFx { insert_id: ins, fx, index: None }], false, None).unwrap();
        let to = 8.5 / spb;
        let live = e.render(0.0, to, 0.0, SR).unwrap();
        assert!(rms(&live) > 0.01, "the live render is silent");

        let n_src = e.project().sources.len();
        let (baked_src, baked_clip) = e.bake_rider(clip, &params).unwrap();
        assert_eq!(baked_clip, clip);
        e.apply(&[Op::RemoveFx { fx_id: 9000 }], false, None).unwrap();
        let p = e.project();
        assert_eq!(p.sources.len(), n_src + 1, "the original source stays");
        assert!(p.sources.iter().any(|s| s.id == src));
        let s = p.sources.iter().find(|s| s.id == baked_src).unwrap();
        assert!(std::path::Path::new(&s.path).exists() && s.name.ends_with("(rider)"), "{s:?}");
        let bc = p.clips.iter().find(|x| x.id == clip).unwrap();
        assert_eq!((bc.source, bc.gain, bc.start, bc.len), (baked_src, 0.0, c.start, c.len));
        assert!(bc.offset.abs() < 1e-9, "a whole-frame region start leaves no offset: {}", bc.offset);
        assert_eq!(wave_engine::engine::playback_latency(&p, SR as f32), 0, "no look-ahead left in the mix");
        let baked = e.render(0.0, to, 0.0, SR).unwrap();
        let (i, d) = worst(&live, &baked);
        assert!(d < 1e-4, "look {look}: baked differs from the live rider by {d} at frame {} ({} vs {})", i / 2, live[i], baked[i]);

        // undo the fx removal, then the bake: the clip is back on its source, gain and offset
        assert!(e.undo());
        assert!(e.undo());
        let p = e.project();
        let rc = p.clips.iter().find(|x| x.id == clip).unwrap();
        assert_eq!(rc, &c, "undo restores the clip");
        assert!(p.sources.iter().all(|s| s.id != baked_src), "undo drops the baked source");
        let again = e.render(0.0, to, 0.0, SR).unwrap();
        assert!(worst(&live, &again).1 < 1e-6, "undo restores the live sound");
        e.apply(&[Op::RemoveFx { fx_id: 9000 }], false, None).unwrap();
    }
    assert!(e.bake_rider(424242, &rider_params(0.0)).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

/// Puts a plan's chain on the vocal insert as the UI does.
fn put_chain(e: &Engine, chain: &[wave_engine::autovocal::PlannedFx], first_id: Id) {
    let p = e.project();
    let ins = insert_of(&p, 0);
    let ops: Vec<Op> = chain
        .iter()
        .enumerate()
        .map(|(k, f)| {
            let mut fx = new_fx(first_id + k as Id, f.kind, p.bpm);
            for (id, v) in &f.params {
                fx.params.insert(id.clone(), *v);
            }
            fx.mix = f.mix as f32;
            fx.bypass = !f.enabled;
            Op::PutFx { insert_id: ins, fx, index: None }
        })
        .collect();
    e.apply(&ops, false, None).unwrap();
}

/// Renders the vocal through `live` (rider on the insert), then bakes `rider` and renders
/// through `rest`; undoes everything afterwards. Returns (live, baked) renders.
fn live_vs_baked(e: &Engine, clip: Id, live: &[wave_engine::autovocal::PlannedFx], rider: &BTreeMap<String, f64>, rest: &[wave_engine::autovocal::PlannedFx], to: f64) -> (Vec<f32>, Vec<f32>) {
    put_chain(e, live, 9100);
    let a = e.render(0.0, to, 1.0, SR).unwrap();
    let pdc = wave_engine::engine::playback_latency(&e.project(), SR as f32);
    assert!(pdc > SR as usize, "the live plan delays the mix by its long rider ({pdc} frames)");
    for k in 0..live.len() {
        e.apply(&[Op::RemoveFx { fx_id: 9100 + k as Id }], false, None).unwrap();
    }
    e.bake_rider(clip, rider).unwrap();
    put_chain(e, rest, 9200);
    let pdc = wave_engine::engine::playback_latency(&e.project(), SR as f32);
    assert!(pdc < SR as usize / 10, "the baked plan adds only the short look-aheads ({pdc} frames)");
    let b = e.render(0.0, to, 1.0, SR).unwrap();
    for _ in 0..3 + live.len() {
        assert!(e.undo());
    }
    assert!(e.project().inserts.iter().all(|m| m.fx.is_empty()));
    (a, b)
}

#[test]
fn baked_plan_renders_like_the_live_plan() {
    let (e, clip, _, dir) = vocal_engine("plan", 10.0);
    let spb = 60.0 / e.project().bpm;
    let to = 10.0 / spb;
    let live_plan = e.auto_vocal_plan(clip, None, Style::Natural, false).unwrap();
    let plan = e.auto_vocal_plan(clip, None, Style::Natural, true).unwrap();
    // the baked plan is the live plan minus the rider: same calibration, same metrics
    let rb = plan.rider_bake.clone().expect("rider to bake");
    assert_eq!(rb.kind, FxKind::Rider);
    assert_eq!(rb.params, live_plan.chain[1].params);
    assert_eq!(rb.params["look"], 1.0, "offline the rider keeps its long look-ahead");
    assert!(plan.chain.iter().all(|f| f.kind != FxKind::Rider));
    assert_eq!(plan.chain.len(), live_plan.chain.len() - 1);
    assert_eq!(serde_json::to_value(&plan.stages).unwrap(), serde_json::to_value(&live_plan.stages).unwrap());
    let j = serde_json::to_value(&plan).unwrap();
    assert!(j["riderBake"]["type"] == "rider" && serde_json::to_value(&live_plan).unwrap()["riderBake"].is_null());

    // Pitch correction is not part of the level calibration and baking moves it after the
    // rider (live it runs before it): it stays out of these comparisons.
    let without = |chain: &[wave_engine::autovocal::PlannedFx], kinds: &[FxKind]| -> Vec<wave_engine::autovocal::PlannedFx> {
        chain.iter().cloned().map(|mut f| {
            if kinds.contains(&f.kind) {
                f.enabled = false;
            }
            f
        }).collect()
    };
    // 1. The calibrated dynamics (FET → compressor → de-esser) and the limiter are
    //    time-invariant: the baked chain plays the same samples.
    let off = [FxKind::Tune, FxKind::Reverb];
    let (live, baked) = live_vs_baked(&e, clip, &without(&live_plan.chain, &off), &rb.params, &without(&plan.chain, &off), to);
    let (i, d) = worst(&live, &baked);
    assert!(d < 1e-3, "baked chain differs from the live chain by {d} at frame {}", i / 2);
    // 2. With the reverb: its modulation runs on the effect's own clock, which the live
    //    rider's 1.6 s of PDC shifts against the voice, so compare levels, not samples.
    let off = [FxKind::Tune];
    let (live, baked) = live_vs_baked(&e, clip, &without(&live_plan.chain, &off), &rb.params, &without(&plan.chain, &off), to);
    let mono = |x: &[f32]| x.chunks(2).map(|f| 0.5 * (f[0] + f[1])).collect::<Vec<f32>>();
    let (ll, lb) = (wave_engine::autovocal::integrated_lufs(&mono(&live), SR as f32), wave_engine::autovocal::integrated_lufs(&mono(&baked), SR as f32));
    assert!((ll - lb).abs() < 0.05, "loudness {ll:.3} vs {lb:.3} LUFS");
    let (tl, tb) = (wave_engine::autovocal::true_peak_db(&mono(&live)), wave_engine::autovocal::true_peak_db(&mono(&baked)));
    assert!((tl - tb).abs() < 0.3, "true peak {tl:.2} vs {tb:.2} dBTP");
    let err: f64 = live.iter().zip(&baked).map(|(a, b)| ((a - b) as f64).powi(2)).sum();
    let sig: f64 = live.iter().map(|a| (*a as f64).powi(2)).sum();
    // the difference is the reverb's wet path (−18 dB send) with another modulation phase
    assert!(10.0 * (err / sig).log10() < -10.0, "baked vs live error {:.1} dB", 10.0 * (err / sig).log10());
    println!("live {ll:.3} LUFS {tl:.2} dBTP · baked {lb:.3} LUFS {tb:.2} dBTP · error {:.1} dB", 10.0 * (err / sig).log10());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_rider_added_by_hand_is_live() {
    let fx = new_fx(1, FxKind::Rider, 120.0);
    assert_eq!(fx.params["look"], 2.0);
    assert_eq!(wave_engine::catalog::latency(&fx, SR as f32), 0, "a rider added by hand must not delay the mix");
    for p in wave_engine::catalog::presets(FxKind::Rider) {
        let long = wave_engine::rider::look_mode(p.params.get("look").copied().unwrap_or(0.0) as f32) == wave_engine::rider::look::LONG;
        assert_eq!(long, p.name.ends_with("(długi look-ahead, opóźnienie)"), "preset {}", p.name);
    }
}
