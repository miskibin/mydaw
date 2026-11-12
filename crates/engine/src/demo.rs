//! The project the program starts with (eight tracks, the starter kit in the Channel Rack) and
//! the small fixed song tests and benchmarks build on.

use crate::catalog::{new_fx, load_preset};
use crate::model::*;

const TC: [&str; 10] = ["#4f8ef7", "#5aa9e6", "#69c3d6", "#47b8a8", "#5fbf8a", "#7c8cf8", "#e0a44a", "#9a8cf0", "#d9678f", "#8b93a1"];
const COLORS: [usize; 8] = [0, 6, 5, 3, 4, 8, 9, 9];

fn base(names: &[&str], ins_names: &[&str]) -> Project {
    let mut p = Project {
        version: 2,
        bpm: 128.0,
        tracks: vec![],
        inserts: vec![Insert { id: 0, name: "Master".into(), color: "#d5dae2".into(), vol: VOL_UNITY, pan: 0.0, width: 1.0, muted: false, solo: false, fx: vec![] }],
        patterns: vec![],
        sources: vec![],
        clips: vec![],
        loop_range: None,
        automations: vec![],
        comps: vec![],
        channels: vec![],
        notebook: Notebook::default(),
        next_id: 1000,
    };
    for (i, name) in ins_names.iter().enumerate() {
        p.inserts.push(Insert { id: i as Id + 1, name: (*name).into(), color: TC[COLORS[i]].into(), vol: VOL_UNITY, pan: 0.0, width: 1.0, muted: false, solo: false, fx: vec![] });
    }
    for (i, name) in names.iter().enumerate() {
        p.tracks.push(Track { id: i as Id + 1, name: (*name).into(), color: TC[COLORS[i]].into(), muted: false, solo: false, arm: false, vol: VOL_UNITY, h: 56.0, insert: i as Id + 1, icon: None });
    }
    let limiter = new_fx(900, FxKind::Limit, p.bpm);
    p.inserts[0].fx.push(limiter);
    p
}

/// The empty project with track n sent to insert n (a multitrack session): what tests and
/// benchmarks that put effects on single tracks build on.
pub fn routed_project() -> Project {
    let mut p = base(
        &["Track 1", "Track 2", "Track 3", "Track 4", "Track 5", "Track 6", "Track 7", "Track 8"],
        &["Insert 1", "Insert 2", "Insert 3", "Insert 4", "Insert 5", "Insert 6", "Insert 7", "Insert 8"],
    );
    p.add_starter_kit();
    // one empty pattern, as in FL Studio: the Channel Rack and the Piano roll have something to edit
    p.patterns.push(Pattern { id: 1, name: "Pattern 1".into(), color: TC[0].into(), len: 4.0, min_len: 4.0, notes: vec![], swing: 0.0 });
    p
}

/// What the program starts with. Playlist tracks are plain lanes, as in FL Studio: none is tied
/// to a mixer insert (they play into the master); routing belongs to the channels, and a track
/// is sent to an insert by hand.
pub fn empty_project() -> Project {
    let mut p = routed_project();
    for t in &mut p.tracks {
        t.insert = 0;
    }
    p
}


/// A sketch of a song: three short patterns (beat, hats, bass line) on the starter kit plus
/// a gated 808 bass, with a vocal chain and a drum-bus compressor. Small and fixed: tests,
/// benchmarks and examples build on it.
pub fn sketch_project() -> Project {
    let mut p = base(
        &["Drums", "Hats", "Bass", "Pad", "Perc", "Vocal", "Track 7", "Track 8"],
        &["Drums", "Hats", "Bass", "Pad", "Perc", "Vocal", "Insert 7", "Insert 8"],
    );
    p.tracks[5].arm = true;
    let [kick, clap, hat, _snare] = p.add_starter_kit();
    let bass = p.factory_channel("bass-808.flac").expect("a standard-library file");
    for c in p.channels.iter_mut().filter(|c| c.id == bass) {
        c.name = "Bass".into();
        if let ChannelSrc::Sample { sampler, .. } = &mut c.src {
            sampler.gate = true;
        }
    }
    let n = |c: Id, step: f32, len: f32, key: Option<u8>| Note::new(c, step, len, key);
    p.patterns = vec![
        Pattern {
            id: 1,
            name: "Beat".into(),
            color: "#4f8ef7".into(),
            len: 4.0,
            min_len: 4.0,
            notes: [(0., kick), (4., kick), (8., kick), (12., kick), (10., kick), (4., clap), (12., clap)]
                .iter()
                .map(|&(s, i)| n(i, s, 1.0, None))
                .collect(),
            swing: 0.0,
        },
        Pattern {
            id: 2,
            name: "Hats".into(),
            color: "#e0a44a".into(),
            len: 4.0,
            min_len: 4.0,
            notes: [2., 6., 10., 14., 15., 7.].iter().map(|&s| n(hat, s, 1.0, None)).collect(),
            swing: 0.0,
        },
        Pattern {
            id: 3,
            name: "Bass".into(),
            color: "#7c8cf8".into(),
            len: 8.0,
            min_len: 4.0,
            notes: [(0., 3., 33), (4., 2., 33), (6., 2., 40), (8., 4., 36), (14., 2., 38), (16., 3., 29), (20., 2., 29), (22., 2., 36), (24., 6., 31), (30., 2., 34)]
                .iter()
                .map(|&(s, l, k)| n(bass, s, l, Some(k)))
                .collect(),
            swing: 0.0,
        },
    ];
    let clip = |id, track, start, len, pat| Clip {
        id, track, start, len, offset: 0.0, kind: ClipKind::Pat, source: pat, muted: false, gain: 0.0,
        fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None, stretch: 1.0, resample: false,
    };
    p.clips = vec![clip(1, 0, 0.0, 16.0, 1), clip(2, 0, 16.0, 16.0, 1), clip(3, 1, 8.0, 24.0, 2), clip(4, 2, 0.0, 32.0, 3)];
    // Vocal chain: EQ (custom) → comp → reverb, drum bus comp.
    let mut eq = new_fx(902, FxKind::Eq, p.bpm);
    load_preset(&mut eq, 1, p.bpm);
    p.inserts[6].fx = vec![eq, new_fx(903, FxKind::Comp, p.bpm), new_fx(904, FxKind::Reverb, p.bpm)];
    let mut comp = new_fx(905, FxKind::Comp, p.bpm);
    load_preset(&mut comp, 2, p.bpm);
    p.inserts[1].fx = vec![comp];
    p
}
