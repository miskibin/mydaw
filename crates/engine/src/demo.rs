//! The starting song: the prototype's demo arrangement (drums, hats, bass) on a full mixer.

use crate::catalog::{new_fx, load_preset};
use crate::model::*;

const TC: [&str; 10] = ["#4f8ef7", "#5aa9e6", "#69c3d6", "#47b8a8", "#5fbf8a", "#7c8cf8", "#e0a44a", "#9a8cf0", "#d9678f", "#8b93a1"];
const COLORS: [usize; 8] = [0, 6, 5, 3, 4, 8, 9, 9];

fn base(names: &[&str], ins_names: &[&str]) -> Project {
    let mut p = Project {
        version: 2,
        bpm: 128.0,
        tracks: vec![],
        inserts: vec![Insert { id: 0, name: "Master".into(), color: "#d5dae2".into(), vol: VOL_UNITY, pan: 0.0, muted: false, solo: false, fx: vec![] }],
        patterns: vec![],
        sources: vec![],
        clips: vec![],
        loop_range: None,
        next_id: 1000,
    };
    for (i, name) in ins_names.iter().enumerate() {
        p.inserts.push(Insert { id: i as Id + 1, name: (*name).into(), color: TC[COLORS[i]].into(), vol: VOL_UNITY, pan: 0.0, muted: false, solo: false, fx: vec![] });
    }
    for (i, name) in names.iter().enumerate() {
        p.tracks.push(Track { id: i as Id + 1, name: (*name).into(), color: TC[COLORS[i]].into(), muted: false, solo: false, arm: false, vol: VOL_UNITY, h: 56.0, insert: i as Id + 1 });
    }
    let limiter = new_fx(900, FxKind::Limit, p.bpm);
    p.inserts[0].fx.push(limiter);
    p
}

pub fn empty_project() -> Project {
    base(
        &["Track 1", "Track 2", "Track 3", "Track 4", "Track 5", "Track 6", "Track 7", "Track 8"],
        &["Insert 1", "Insert 2", "Insert 3", "Insert 4", "Insert 5", "Insert 6", "Insert 7", "Insert 8"],
    )
}

pub fn demo_project() -> Project {
    let mut p = base(
        &["Drums", "Hats", "Bass", "Pad", "Perc", "Vocal", "Track 7", "Track 8"],
        &["Drums", "Hats", "Bass", "Pad", "Perc", "Vocal", "Insert 7", "Insert 8"],
    );
    p.tracks[5].arm = true;
    let n = |inst, step: f32, len: f32, key: Option<u8>| Note { inst, step, len, p: key };
    p.patterns = vec![
        Pattern {
            id: 1,
            name: "Beat".into(),
            color: "#4f8ef7".into(),
            len: 4.0,
            notes: [(0., Inst::Kick), (4., Inst::Kick), (8., Inst::Kick), (12., Inst::Kick), (10., Inst::Kick), (4., Inst::Clap), (12., Inst::Clap)]
                .iter()
                .map(|&(s, i)| n(i, s, 1.0, None))
                .collect(),
        },
        Pattern {
            id: 2,
            name: "Hats".into(),
            color: "#e0a44a".into(),
            len: 4.0,
            notes: [2., 6., 10., 14., 15., 7.].iter().map(|&s| n(Inst::Hat, s, 1.0, None)).collect(),
        },
        Pattern {
            id: 3,
            name: "Bass".into(),
            color: "#7c8cf8".into(),
            len: 8.0,
            notes: [(0., 3., 33), (4., 2., 33), (6., 2., 40), (8., 4., 36), (14., 2., 38), (16., 3., 29), (20., 2., 29), (22., 2., 36), (24., 6., 31), (30., 2., 34)]
                .iter()
                .map(|&(s, l, k)| n(Inst::Bass, s, l, Some(k)))
                .collect(),
        },
    ];
    let clip = |id, track, start, len, pat| Clip {
        id, track, start, len, offset: 0.0, kind: ClipKind::Pat, source: pat, muted: false, gain: 0.0,
        fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None,
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
