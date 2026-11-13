//! "Make unique" for audio clips: the clip gets its own copy of the source on a new track that
//! plays where the old track does (no mixer insert is claimed), in one undo step, without
//! touching other clips.

use std::path::PathBuf;
use std::sync::Arc;

use wave_engine::*;

const SR: u32 = 48000;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// A 1 s constant-level stereo file.
fn tone(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("wave-unique-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("guitar.wav");
    let data: Vec<f32> = (0..SR as usize).flat_map(|_| [0.3f32, 0.3]).collect();
    wave_engine::sources::write_wav(&wav, &data, SR).unwrap();
    (dir, wav)
}

/// No effects anywhere, so levels are exact.
fn dry(mut p: Project) -> Project {
    for m in p.inserts.iter_mut() {
        m.fx.clear();
    }
    p
}

/// The engine with "guitar" at beats 0 and 4 of track 0, and another copy at beat 0 of track 1.
fn setup(p: Project, tag: &str) -> (Arc<Engine>, PathBuf, Id, [Id; 3]) {
    let (dir, wav) = tone(tag);
    let e = Engine::new(dry(p), Arc::new(NoPlugins));
    let (srcs, clips) = e.import_audio(&[wav], Some((0, 0.0))).unwrap();
    let a = e
        .project()
        .clips
        .iter()
        .find(|c| c.id == clips[0])
        .cloned()
        .unwrap();
    let b = Clip {
        id: 70_001,
        start: 4.0,
        ..a.clone()
    };
    let c = Clip {
        id: 70_002,
        track: 1,
        ..a.clone()
    };
    e.apply(&[Op::PutClips { clips: vec![b, c] }], false, None)
        .unwrap();
    (e, dir, srcs[0], [a.id, 70_001, 70_002])
}

fn clip(e: &Engine, id: Id) -> Clip {
    e.project().clips.into_iter().find(|c| c.id == id).unwrap()
}

#[test]
fn the_clip_gets_its_own_source_and_track_with_the_old_routing() {
    let (e, dir, src, [a, b, c]) = setup(Project::empty(), "basic");
    let before = e.project();
    let n_tracks = before.tracks.len();
    let n_inserts = before.inserts.len();
    let r = e.make_unique_audio(&[a]).unwrap();
    let p = e.project();

    // a copy of the source: same file and length, its own id and numbered name
    assert_ne!(r.source_id, src);
    let old = p.sources.iter().find(|s| s.id == src).unwrap();
    let new = p.sources.iter().find(|s| s.id == r.source_id).unwrap();
    assert_eq!(new.name, "guitar #2");
    assert_eq!(
        (&new.path, new.dur, &new.color),
        (&old.path, old.dur, &old.color)
    );
    assert_eq!(old.name, "guitar", "the original keeps its name");

    // a new track right below, playing into the master like the old one: no insert is claimed
    assert_eq!(r.track, 1);
    assert_eq!(p.tracks.len(), n_tracks + 1);
    assert_eq!(p.inserts, before.inserts, "the mixer is untouched");
    assert_eq!(p.inserts.len(), n_inserts);
    let t = &p.tracks[1];
    assert_eq!((t.name.as_str(), t.insert), ("guitar #2", 0));
    assert_eq!(r.insert_id, 0);

    // only the given clip moved; the clip that was on track 1 is now on track 2
    let ca = clip(&e, a);
    assert_eq!((ca.track, ca.source, ca.start), (1, r.source_id, 0.0));
    assert_eq!((clip(&e, b).track, clip(&e, b).source), (0, src));
    assert_eq!((clip(&e, c).track, clip(&e, c).source), (2, src));
    assert_eq!(
        p.tracks[2].id, before.tracks[1].id,
        "tracks below keep their ids and contents"
    );

    // one undo step restores everything, redo brings it back
    assert!(e.undo());
    assert_eq!(e.project(), before);
    assert!(e.redo());
    assert_eq!(e.project(), p);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_unique_clip_keeps_the_old_tracks_insert() {
    // Project::routed sends track n to insert n: the user's routing carries over
    let (e, dir, _, [a, b, c]) = setup(Project::routed(), "route");
    e.apply(&[Op::RemoveClips { ids: vec![b, c] }], false, None)
        .unwrap();
    let spb = 60.0 / e.project().bpm;
    let at = |beat: f64| (beat * spb * SR as f64) as usize * 2;
    let level = |e: &Engine| {
        let out = e.render(0.0, 1.0, 0.0, SR).unwrap();
        rms(&out[at(0.2)..at(0.8)])
    };
    let dry_level = level(&e);
    assert!(dry_level > 0.2, "the clip plays ({dry_level})");
    let n = e.project().inserts.len();
    let old_insert = e.project().tracks[0].insert;
    let r = e.make_unique_audio(&[a]).unwrap();
    assert_eq!(r.insert_id, old_insert);
    assert_eq!(e.project().tracks[1].insert, old_insert);
    assert_eq!(e.project().inserts.len(), n, "no insert added");
    assert!(
        (level(&e) - dry_level).abs() < 1e-3,
        "same sound after making it unique"
    );
    // muting the old track's insert silences it: it really plays through there
    let mut m = e
        .project()
        .inserts
        .into_iter()
        .find(|m| m.id == old_insert)
        .unwrap();
    m.muted = true;
    e.apply(&[Op::PutInsert { insert: m }], false, None)
        .unwrap();
    assert!(level(&e) < 1e-6, "muted insert still sounds");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn several_clips_move_together_and_names_count_up() {
    let (e, dir, _, [a, b, c]) = setup(Project::empty(), "names");
    let r = e.make_unique_audio(&[a, b]).unwrap();
    assert_eq!((clip(&e, a).track, clip(&e, b).track), (1, 1));
    assert_eq!(clip(&e, a).source, clip(&e, b).source);
    // the third copy, from the original and from the copy alike, is "#3", then "#4"
    let r2 = e.make_unique_audio(&[c]).unwrap();
    let r3 = e.make_unique_audio(&[a]).unwrap();
    let name = |id: Id| {
        e.project()
            .sources
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
            .name
    };
    assert_eq!(
        (name(r.source_id), name(r2.source_id), name(r3.source_id)),
        ("guitar #2".into(), "guitar #3".into(), "guitar #4".into())
    );
    // none claimed an insert: every new track plays into the master like its old one
    assert_eq!([r.insert_id, r2.insert_id, r3.insert_id], [0, 0, 0]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn refuses_what_it_cannot_do_and_changes_nothing() {
    let (e, dir, _, [a, _, c]) = setup(Project::empty(), "refuse");
    let before = e.project();
    assert!(e.make_unique_audio(&[]).is_err());
    assert!(e.make_unique_audio(&[999_999]).is_err());
    assert!(
        e.make_unique_audio(&[a, c]).is_err(),
        "clips on different tracks"
    );
    let pat = Clip {
        id: 70_100,
        kind: ClipKind::Pat,
        source: 1,
        start: 16.0,
        len: 4.0,
        ..clip(&e, a)
    };
    e.apply(&[Op::PutClips { clips: vec![pat] }], false, None)
        .unwrap();
    let before_pat = e.project();
    assert!(e.make_unique_audio(&[70_100]).is_err(), "a pattern clip");
    assert_eq!(e.project(), before_pat);
    assert!(e.undo());
    assert_eq!(e.project(), before);
    let _ = std::fs::remove_dir_all(dir);
}
