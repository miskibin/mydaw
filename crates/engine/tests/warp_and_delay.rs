//! Warp's maps reach the running audio thread without a graph rebuild, ops refuse bad maps,
//! and a project with the first delay loads as the same echo.

use std::sync::Arc;

use wave_engine::catalog::new_fx;
use wave_engine::*;

const SR: u32 = 48000;
const BLOCK: usize = 256;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
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

/// A steady tone on track 2, nothing else; the engine playing from 0.
fn setup(tag: &str) -> (Arc<Engine>, Rt) {
    let mut p = Project::routed();
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    let e = Engine::new(p, Arc::new(NoPlugins));
    let dir = std::env::temp_dir().join(format!("wave-warp-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("tone.wav");
    let x: Vec<f32> = (0..SR as usize * 2 * 10)
        .map(|i| 0.3 * ((i / 2) as f32 * 0.03).sin())
        .collect();
    sources::write_wav(&wav, &x, SR).unwrap();
    e.import_audio(std::slice::from_ref(&wav), Some((2, 0.0)))
        .unwrap();
    let rt = e.create_rt(SR as f32, BLOCK);
    e.play(0.0);
    (e, rt)
}

fn put(e: &Engine, fx: Fx) -> Result<u64, EngineError> {
    let ins = e.project().tracks[2].insert;
    e.apply(
        &[Op::PutFx {
            insert_id: ins,
            fx,
            index: None,
        }],
        false,
        None,
    )
}

#[test]
fn editing_a_warp_map_reaches_the_audio_thread() {
    let (e, mut rt) = setup("live");
    let bpm = e.project().bpm;
    put(&e, new_fx(9100, FxKind::Warp, bpm)).unwrap();
    let before = rms(&run(&mut rt, SR as usize));
    assert!(
        before > 0.05,
        "the tone plays through the factory slot 1 ({before})"
    );
    // draw volume slot 1 flat at zero: only the maps change (no graph rebuild, no new processor)
    let mut f = e
        .project()
        .inserts
        .iter()
        .flat_map(|m| m.fx.clone())
        .find(|f| f.id == 9100)
        .unwrap();
    f.maps.as_mut().unwrap().vol[0].points = vec![AutoPoint::at(0.0, 0.0), AutoPoint::at(4.0, 0.0)];
    put(&e, f).unwrap();
    let _ = run(&mut rt, BLOCK * 4);
    let after = rms(&run(&mut rt, SR as usize / 2));
    assert!(after < 1e-4, "the edited map is heard at once ({after})");
}

#[test]
fn ops_refuse_bad_warp_maps() {
    let (e, _rt) = setup("ops");
    let bpm = e.project().bpm;
    let mut f = new_fx(9200, FxKind::Warp, bpm);
    f.maps = None;
    assert!(put(&e, f).is_err(), "a Warp needs maps");
    let mut f = new_fx(9200, FxKind::Warp, bpm);
    f.maps.as_mut().unwrap().time[0].points = vec![AutoPoint::at(0.0, 1.5)];
    assert!(put(&e, f).is_err(), "v outside 0..1");
    let mut f = new_fx(9200, FxKind::Warp, bpm);
    f.maps.as_mut().unwrap().time[0].points =
        vec![AutoPoint::at(3.0, 1.0), AutoPoint::at(1.0, 1.0)];
    assert!(put(&e, f).is_err(), "points out of order");
    let mut f = new_fx(9201, FxKind::Delay, bpm);
    f.maps = Some(Default::default());
    assert!(put(&e, f).is_err(), "only a Warp has maps");
    assert!(put(&e, new_fx(9202, FxKind::Warp, bpm)).is_ok());
}

#[test]
fn warp_reports_its_latency() {
    let f = new_fx(1, FxKind::Warp, 120.0);
    assert_eq!(catalog::latency(&f, 48000.0), dsp::warp::LATENCY);
}

#[test]
fn a_project_with_the_first_delay_loads_as_the_same_echo() {
    // the first delay stored only time / fb / mix, and automation of time in its 10..1500 ms range
    let mut p = Project::routed();
    let ins = p.tracks[2].insert;
    let old = Fx {
        id: 9300,
        kind: FxKind::Delay,
        bypass: false,
        mix: 1.0,
        preset: Some(0),
        params: [
            ("time".to_string(), 300.0),
            ("fb".to_string(), 50.0),
            ("mix".to_string(), 40.0),
        ]
        .into_iter()
        .collect(),
        bands: None,
        plugin: None,
        maps: None,
    };
    p.inserts.iter_mut().find(|m| m.id == ins).unwrap().fx = vec![old];
    let v_old = (300f32 / 10.0).ln() / 150f32.ln();
    p.automations.push(Automation {
        id: 9301,
        name: "t".into(),
        color: "#fff".into(),
        target: AutoTarget::Fx {
            fx_id: 9300,
            param: "time".into(),
        },
        points: vec![AutoPoint::at(0.0, v_old)],
    });
    let e = Engine::new(p, Arc::new(NoPlugins));
    let p = e.project();
    let f = p
        .inserts
        .iter()
        .flat_map(|m| m.fx.iter())
        .find(|f| f.id == 9300)
        .unwrap();
    assert_eq!(f.params["sync"], 1.0, "free time, as before");
    assert_eq!(f.params["time"], 300.0);
    assert_eq!(f.params["fb"], 50.0);
    assert_eq!(f.params["mix"], 40.0);
    assert_eq!(f.params["ftype"], 0.0, "no loop filter");
    let v = p.automations[0].points[0].v;
    assert!(
        (2000f32.powf(v) - 300.0).abs() < 0.5,
        "automation still lands on 300 ms"
    );
    // loading it again changes nothing
    let mut again = p.clone();
    again.migrate();
    assert_eq!(again.automations, p.automations);
}
