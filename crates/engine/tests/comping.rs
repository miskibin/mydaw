//! Take lanes and comping: loop recording into takes, comp playback with crossfades,
//! undo, persistence and flattening.

use std::path::PathBuf;
use std::sync::Arc;

use wave_engine::*;

const SR: u32 = 48000;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wave-comp-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An empty project with no effects anywhere, so levels are exact.
fn dry_project() -> Project {
    let mut p = Project::empty();
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    p
}

/// Left channel of an interleaved render.
fn left(x: &[f32]) -> Vec<f32> {
    x.chunks(2).map(|f| f[0]).collect()
}

fn frame_at(beat: f64, bpm: f64) -> usize {
    (beat * 60.0 / bpm * SR as f64).round() as usize
}

/// Three constant-level takes (0.2, 0.4, 0.6) over beats 0..8, imported as sources.
fn three_takes(e: &Engine, dir: &std::path::Path) -> Vec<Id> {
    let bpm = e.project().bpm;
    let secs = 8.0 * 60.0 / bpm + 0.5;
    let mut paths = vec![];
    for (k, lvl) in [0.2f32, 0.4, 0.6].iter().enumerate() {
        let path = dir.join(format!("take{k}.wav"));
        let data: Vec<f32> = (0..(secs * SR as f64) as usize).flat_map(|_| [*lvl, *lvl]).collect();
        wave_engine::sources::write_wav(&path, &data, SR).unwrap();
        paths.push(path);
    }
    e.import_audio(&paths, None).unwrap().0
}

fn comp_with(srcs: &[Id], regions: Vec<CompRegion>) -> Comp {
    Comp {
        id: 9000,
        name: "Vox comp".into(),
        takes: srcs.iter().enumerate().map(|(k, &s)| Take { id: 9001 + k as Id, name: format!("Take {}", k + 1), source: s, start: 0.0, len: 8.0, offset: 0.0, color: None }).collect(),
        regions,
        xfade: DEFAULT_XFADE,
    }
}

fn comp_clip(start: f64) -> Clip {
    Clip { id: 9100, track: 0, start, len: 8.0, offset: 0.0, kind: ClipKind::Comp, source: 9000, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None, stretch: 1.0, resample: false, pitch: 0.0 }
}

fn r(take: Id, from: f64, to: f64) -> CompRegion {
    CompRegion { take, from, to, xfade: None }
}

#[test]
fn five_loop_passes_make_five_takes_and_the_newest_plays() {
    let mut p = dry_project();
    for t in p.tracks.iter_mut() {
        t.arm = false;
    }
    p.tracks[0].arm = true;
    p.loop_range = Some(LoopRange { a: 8.0, b: 12.0 });
    let bpm = p.bpm;
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut rt = e.create_rt(SR as f32, 256);
    e.set_transport(None, Some(false), Some(true), None);
    e.play(8.0);
    let pass = frame_at(4.0, bpm);
    // pass k is a sine at amplitude 0.1·(k+1); stop a few hundred samples into a 6th pass
    let total = pass * 5 + 300;
    let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
    let mut done = 0;
    while done < total {
        let n = 256.min(total - done);
        let inp: Vec<f32> = (0..n).map(|i| {
            let f = done + i;
            (f as f32 * 0.05).sin() * 0.1 * (f / pass + 1) as f32
        }).collect();
        rt.process(&mut l[..n], &mut r[..n], Some((&inp, &inp)));
        done += n;
    }
    e.stop();
    let p = e.project();
    assert_eq!(p.comps.len(), 1, "one take group");
    let comp = &p.comps[0];
    assert_eq!(comp.takes.len(), 5, "one take per pass (the 300-sample tail is not a take)");
    for t in &comp.takes {
        assert!((t.len - 4.0).abs() < 0.01, "take length {}", t.len);
        assert!((t.start - comp.takes[0].start).abs() < 1e-9);
    }
    let newest = comp.takes.last().unwrap();
    assert_eq!(comp.regions, vec![CompRegion { take: newest.id, from: newest.start, to: newest.end(), xfade: None }]);
    let clip = p.clips.iter().find(|c| c.kind == ClipKind::Comp).expect("a comp clip");
    assert_eq!(clip.track, 0);
    assert!((clip.start - 8.0).abs() < 1e-9 && (clip.len - 4.0).abs() < 0.01);
    // the newest pass (amplitude 0.5) is what plays
    let out = left(&e.render(8.0, 12.0, 0.0, SR).unwrap());
    let level = rms(&out[1000..out.len() - 1000]);
    assert!((level - 0.5 / 2f32.sqrt()).abs() < 0.01, "comp plays rms {level}, want the 5th pass");
    // one undo step removes the whole recording
    assert!(e.undo());
    assert!(e.project().comps.is_empty() && e.project().clips.iter().all(|c| c.kind != ClipKind::Comp));
}

#[test]
fn a_last_pass_cut_short_leaves_the_rest_to_the_previous_one() {
    let mut p = dry_project();
    p.sources = (1..=3).map(|id| Source { id, name: format!("Take {id}"), color: "#e05263".into(), path: String::new(), dur: 2.0, sr: SR, ch: 1, peak_db: 0.0 }).collect();
    wave_engine::comp::land_takes(&mut p, 0, &[(1, 8.0, 4.0), (2, 8.0, 4.0), (3, 8.0, 1.5)]);
    let c = &p.comps[0];
    let (t2, t3) = (c.takes[1].id, c.takes[2].id);
    assert_eq!(c.regions, vec![r(t3, 0.0, 1.5), r(t2, 1.5, 4.0)]);
}

#[test]
fn recording_over_a_take_adds_a_take_instead_of_overwriting() {
    let mut p = dry_project();
    p.tracks[0].arm = true;
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut rt = e.create_rt(SR as f32, 256);
    e.set_transport(None, Some(false), Some(true), None);
    let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
    let tone = vec![0.25f32; 256];
    for _ in 0..2 {
        e.play(4.0);
        for _ in 0..(SR as usize / 256) {
            rt.process(&mut l, &mut r, Some((&tone, &tone)));
        }
        e.stop();
    }
    let p = e.project();
    assert_eq!(p.clips.iter().filter(|c| c.kind == ClipKind::Audio).count(), 0, "the first take became part of the comp");
    assert_eq!(p.comps.len(), 1);
    assert_eq!(p.comps[0].takes.len(), 2);
    let newest = p.comps[0].takes[1].id;
    assert!(p.comps[0].regions.iter().all(|r| r.take == newest));
}

#[test]
fn comp_of_three_takes_plays_each_region_with_crossfades() {
    let dir = tmp("three");
    let e = Engine::new(dry_project(), Arc::new(NoPlugins));
    let srcs = three_takes(&e, &dir);
    let comp = comp_with(&srcs, vec![r(9001, 0.0, 2.0), r(9002, 2.0, 5.0), r(9003, 5.0, 8.0)]);
    e.apply(&[Op::PutComp { comp }, Op::PutClips { clips: vec![comp_clip(4.0)] }], false, None).unwrap();
    let bpm = e.project().bpm;
    let out = left(&e.render(0.0, 14.0, 0.0, SR).unwrap());
    let at = |b: f64| out[frame_at(b, bpm)];
    // silence before the clip, then each region's own take
    assert!(at(3.9).abs() < 1e-6);
    for (beat, want) in [(5.0, 0.2), (7.5, 0.4), (10.5, 0.6)] {
        assert!((at(beat) - want).abs() < 1e-4, "beat {beat}: {} != {want}", at(beat));
    }
    // the boundaries are equal-power crossfades: no step, and the midpoint is a·cos45 + b·sin45
    for (b, x, y) in [(6.0, 0.2f32, 0.4f32), (9.0, 0.4, 0.6)] {
        let f = frame_at(b, bpm);
        let w = (0.006 * SR as f32) as usize;
        let jump = out[f - w..f + w].windows(2).map(|p| (p[1] - p[0]).abs()).fold(0f32, f32::max);
        assert!(jump < 0.003, "step of {jump} at beat {b}");
        let mid = (x + y) * std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[f] - mid).abs() < 0.01, "crossfade midpoint {} != {mid}", out[f]);
        // the fade is 10 ms long: 6 ms out, each side is clean again
        assert!((out[f - w] - x).abs() < 1e-4 && (out[f + w] - y).abs() < 1e-4);
    }
    // no discontinuity anywhere inside the clip (beyond its hard start/end)
    let (a, z) = (frame_at(4.0, bpm) + 2, frame_at(12.0, bpm) - 2);
    let worst = out[a..z].windows(2).map(|p| (p[1] - p[0]).abs()).fold(0f32, f32::max);
    assert!(worst < 0.003, "largest sample step inside the comp {worst}");
    // a longer crossfade on one boundary
    let mut comp = e.project().comps[0].clone();
    comp.regions[1].xfade = Some(0.05);
    e.apply(&[Op::PutComp { comp }], false, None).unwrap();
    let out = left(&e.render(0.0, 14.0, 0.0, SR).unwrap());
    let f = frame_at(6.0, bpm);
    assert!((out[f - (0.02 * SR as f32) as usize] - 0.2).abs() > 0.01, "50 ms fade not applied");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn comp_edits_undo_and_redo() {
    let dir = tmp("undo");
    let e = Engine::new(dry_project(), Arc::new(NoPlugins));
    let srcs = three_takes(&e, &dir);
    let comp = comp_with(&srcs, vec![r(9001, 0.0, 8.0)]);
    e.apply(&[Op::PutComp { comp: comp.clone() }, Op::PutClips { clips: vec![comp_clip(0.0)] }], false, None).unwrap();
    // swipe take 2 over 2..4
    let mut swiped = comp.clone();
    swiped.write(9002, 2.0, 4.0);
    assert_eq!(swiped.regions, vec![r(9001, 0.0, 2.0), r(9002, 2.0, 4.0), r(9001, 4.0, 8.0)]);
    e.apply(&[Op::PutComp { comp: swiped.clone() }], false, None).unwrap();
    // swipe take 1 back over it: the pieces merge again
    let mut back = swiped.clone();
    back.write(9001, 1.0, 5.0);
    assert_eq!(back.regions, vec![r(9001, 0.0, 8.0)]);
    let bpm = e.project().bpm;
    let lvl = |e: &Engine| left(&e.render(0.0, 8.0, 0.0, SR).unwrap())[frame_at(3.0, bpm)];
    assert!((lvl(&e) - 0.4).abs() < 1e-4);
    assert!(e.undo());
    assert_eq!(e.project().comps[0].regions, comp.regions);
    assert!((lvl(&e) - 0.2).abs() < 1e-4);
    assert!(e.redo());
    assert_eq!(e.project().comps[0].regions, swiped.regions);
    // invalid comps are refused and change nothing
    let mut bad = swiped.clone();
    bad.regions = vec![r(9001, 0.0, 3.0), r(9002, 2.0, 4.0)];
    assert!(e.apply(&[Op::PutComp { comp: bad }], false, None).is_err());
    let mut bad = swiped.clone();
    bad.regions = vec![r(4242, 0.0, 3.0)];
    assert!(e.apply(&[Op::PutComp { comp: bad }], false, None).is_err());
    assert_eq!(e.project().comps[0].regions, swiped.regions);
    // removing a take's source removes the take and its regions
    e.apply(&[Op::RemoveSource { id: srcs[1] }], false, None).unwrap();
    let c = &e.project().comps[0];
    assert_eq!(c.takes.len(), 2);
    assert!(c.regions.iter().all(|x| x.take != 9002));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn takes_and_comps_survive_save_and_open() {
    let dir = tmp("save");
    let e = Engine::new(dry_project(), Arc::new(NoPlugins));
    let srcs = three_takes(&e, &dir);
    let mut comp = comp_with(&srcs, vec![r(9001, 0.0, 3.0), CompRegion { take: 9003, from: 3.0, to: 8.0, xfade: Some(0.02) }]);
    comp.xfade = 0.005;
    e.apply(&[Op::PutComp { comp }, Op::PutClips { clips: vec![comp_clip(2.0)] }], false, None).unwrap();
    let path = dir.join("song.wave");
    e.save(&path).unwrap();
    let f = Engine::new(Project::empty(), Arc::new(NoPlugins));
    assert!(f.open(&path).unwrap().is_empty());
    assert_eq!(f.project(), e.project());
    let (a, b) = (e.render(0.0, 12.0, 0.0, SR).unwrap(), f.render(0.0, 12.0, 0.0, SR).unwrap());
    assert_eq!(a, b, "the reopened comp sounds different");
    // projects from before take lanes still open
    let text = std::fs::read_to_string(&path).unwrap();
    let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    doc["project"].as_object_mut().unwrap().remove("comps");
    doc["project"]["clips"] = serde_json::json!([]);
    std::fs::write(&path, doc.to_string()).unwrap();
    assert!(f.open(&path).is_ok());
    assert!(f.project().comps.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn flatten_renders_the_comp_to_one_audio_clip() {
    let dir = tmp("flat");
    let e = Engine::new(dry_project(), Arc::new(NoPlugins));
    let srcs = three_takes(&e, &dir);
    let comp = comp_with(&srcs, vec![r(9001, 0.0, 2.0), r(9002, 2.0, 5.0), r(9003, 5.5, 8.0)]);
    let mut clip = comp_clip(4.0);
    clip.gain = -6.0;
    e.apply(&[Op::PutComp { comp }, Op::PutClips { clips: vec![clip] }], false, None).unwrap();
    let before = e.render(0.0, 14.0, 0.0, SR).unwrap();
    let src = e.flatten_comp(9100).unwrap();
    let p = e.project();
    assert!(p.comps.is_empty(), "the takes go with the flatten");
    let c = p.clips.iter().find(|c| c.id == 9100).unwrap();
    assert_eq!((c.kind, c.source, c.gain, c.offset), (ClipKind::Audio, src, -6.0, 0.0));
    let after = e.render(0.0, 14.0, 0.0, SR).unwrap();
    let (worst, diff) = before.iter().zip(&after).map(|(a, b)| (a - b).abs()).enumerate().fold((0, 0f32), |m, (i, d)| if d > m.1 { (i, d) } else { m });
    assert!(diff < 1e-4, "flattened audio differs by {diff} at frame {} ({} vs {})", worst / 2, before[worst], after[worst]);
    assert!(e.undo());
    assert_eq!(e.project().comps.len(), 1);
    assert_eq!(e.project().clips.iter().find(|c| c.id == 9100).unwrap().kind, ClipKind::Comp);
    assert!(e.flatten_comp(4242).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn audition_plays_one_take_live_only() {
    let dir = tmp("aud");
    let e = Engine::new(dry_project(), Arc::new(NoPlugins));
    let srcs = three_takes(&e, &dir);
    let comp = comp_with(&srcs, vec![r(9001, 0.0, 8.0)]);
    e.apply(&[Op::PutComp { comp }, Op::PutClips { clips: vec![comp_clip(0.0)] }], false, None).unwrap();
    let mut rt = e.create_rt(SR as f32, 256);
    let rev = e.revision();
    e.audition(9000, Some(9003)).unwrap();
    assert_eq!(e.state().audition.get(&9000), Some(&9003));
    assert_eq!(e.revision(), rev, "auditioning is not an edit");
    e.play(1.0);
    let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
    for _ in 0..8 {
        rt.process(&mut l, &mut r, None);
    }
    assert!((l[100] - 0.6).abs() < 1e-3, "audition plays take 3, got {}", l[100]);
    e.audition(9000, None).unwrap();
    for _ in 0..2 {
        rt.process(&mut l, &mut r, None);
    }
    assert!((l[100] - 0.2).abs() < 1e-3, "back to the comp, got {}", l[100]);
    assert!(e.audition(9000, Some(1)).is_err());
    e.stop();
    let _ = std::fs::remove_dir_all(dir);
}
