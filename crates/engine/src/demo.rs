//! The projects the program starts with: an empty one (eight tracks, the starter kit in the
//! Channel Rack) and the demo song, which shows what a finished arrangement looks like.

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

pub fn empty_project() -> Project {
    let mut p = base(
        &["Track 1", "Track 2", "Track 3", "Track 4", "Track 5", "Track 6", "Track 7", "Track 8"],
        &["Insert 1", "Insert 2", "Insert 3", "Insert 4", "Insert 5", "Insert 6", "Insert 7", "Insert 8"],
    );
    p.add_starter_kit();
    p
}


/// A sketch of a song: three short patterns (beat, hats, bass line) on the starter kit plus
/// a gated 808 bass, with a vocal chain and a drum-bus compressor. Small and fixed: tests,
/// benchmarks and examples build on it, so the demo song can grow without moving them.
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

/// Fader position for a gain in dB (the inverse of [`fader_gain`]).
fn fader(db: f32) -> f32 {
    (10f32.powf(db / 20.0) / 2.0).cbrt().clamp(0.0, 1.0)
}

/// Beats per minute of the demo. One bar is 60 / 124 × 4 = 1.935 s, which the reversed
/// crash below is cut to.
const DEMO_BPM: f64 = 124.0;

/// The demo song: forty bars of house in A minor, built from the standard library the way a
/// user would build it. It is meant to be taken apart, so it uses a bit of everything:
///
/// * drum samplers on a shared drum bus, the closed hat cutting the open one (cut group);
/// * samples played as instruments: the 808 bass in gate mode with a release, a marimba hook,
///   congas at different keys, a crash trimmed and reversed into a riser;
/// * the same crash as plain audio clips on the playlist (one source, two uses);
/// * synth channels for the pad, the stabs and the lead;
/// * swing, velocities, a fill, automation clips (filter sweep, reverb throw, fade-out);
/// * an effect chain per insert and a limiter on the master.
pub fn demo_project() -> Project {
    const TRACKS: [&str; 12] = ["Kick", "Clap", "Hats", "Perc", "Bass", "Pad", "Stabs", "Marimba", "Lead", "FX", "Automation", "Vocal"];
    const INSERTS: [&str; 8] = ["Drums", "Bass", "Pad", "Stabs", "Marimba", "Lead", "FX", "Vocal"];
    let mut p = base(&[], &INSERTS);
    p.bpm = DEMO_BPM;
    let insert = |name: &str| INSERTS.iter().position(|n| *n == name).map(|i| i as Id + 1).expect("a demo insert");
    // the four drum tracks play into one bus; the automation lane has no sound of its own
    let route = ["Drums", "Drums", "Drums", "Drums", "Bass", "Pad", "Stabs", "Marimba", "Lead", "FX", "FX", "Vocal"];
    for (i, name) in TRACKS.iter().enumerate() {
        let color = TC[[0, 8, 6, 4, 5, 3, 2, 7, 1, 9, 9, 8][i]];
        p.tracks.push(Track { id: i as Id + 1, name: (*name).into(), color: color.into(), muted: false, solo: false, arm: i == 11, vol: VOL_UNITY, h: 56.0, insert: insert(route[i]), icon: None });
    }
    let track = |name: &str| TRACKS.iter().position(|n| *n == name).expect("a demo track") as u32;

    // ---- the Channel Rack
    let [kick, clap, hat, snare] = p.add_starter_kit();
    let sampler = |p: &mut Project, file: &str, name: &str, f: &dyn Fn(&mut SamplerState)| -> Id {
        let id = p.factory_channel(file).expect("a standard-library file");
        let c = p.channels.iter_mut().find(|c| c.id == id).expect("just added");
        c.name = name.into();
        if let ChannelSrc::Sample { sampler, .. } = &mut c.src {
            f(sampler);
        }
        id
    };
    let open_hat = sampler(&mut p, "hat-808-open.flac", "Open Hat", &|s| s.cut = 1);
    let shaker = sampler(&mut p, "shaker.flac", "Shaker", &|_| {});
    let conga = sampler(&mut p, "conga-808-high.flac", "Conga", &|_| {});
    let tom = sampler(&mut p, "tom-808-low.flac", "Tom", &|_| {});
    // held notes: the sample stops with the note and fades over the release
    let bass = sampler(&mut p, "bass-808.flac", "808 Bass", &|s| {
        s.gate = true;
        s.release = 90.0;
    });
    let marimba = sampler(&mut p, "marimba.flac", "Marimba", &|_| {});
    // the first bar of the crash, backwards: it swells into the downbeat
    let riser = sampler(&mut p, "crash-studio.flac", "Reverse Crash", &|s| {
        s.end = (60.0 / DEMO_BPM * 4.0 / 5.0) as f32;
        s.reverse = true;
    });
    // the closed hat shares the open hat's cut group: it chokes it, like a real hi-hat
    for c in p.channels.iter_mut().filter(|c| c.id == hat) {
        if let ChannelSrc::Sample { sampler, .. } = &mut c.src {
            sampler.cut = 1;
        }
    }
    // `out_db`: the patch's output trim (the presets differ a lot in level), so the faders can stay at unity
    let synth = |p: &mut Project, preset: &str, name: &str, color: &str, out_db: f64| -> Id {
        let id = p.alloc_id();
        let mut state = crate::poly::preset_state(preset).expect("a factory synth preset");
        state.params.insert("fx_out".into(), out_db);
        p.channels.push(Channel::new(id, name, color, ChannelSrc::Synth { synth: state }));
        id
    };
    let pad = synth(&mut p, "Pad Ciepły", "Pad", TC[3], -3.0);
    let stab = synth(&mut p, "Keys Dance", "Stabs", TC[2], -1.0);
    let lead = synth(&mut p, "Pluck Szklany", "Lead", TC[1], 9.0);
    let level = |p: &mut Project, id: Id, vol: f32, pan: f32| {
        if let Some(c) = p.channels.iter_mut().find(|c| c.id == id) {
            c.vol = vol;
            c.pan = pan;
        }
    };
    level(&mut p, clap, 0.68, 0.0);
    level(&mut p, hat, 0.6, 0.15);
    level(&mut p, open_hat, 0.58, 0.15);
    level(&mut p, shaker, 0.5, -0.3);
    level(&mut p, conga, 0.6, 0.35);
    level(&mut p, tom, 0.66, -0.2);
    level(&mut p, bass, 0.74, 0.0);
    level(&mut p, marimba, 0.7, -0.1);
    level(&mut p, riser, 0.62, 0.0);

    // ---- patterns (steps are 16ths)
    let n = |ch: Id, step: f32, len: f32, key: Option<u8>, v: f32| Note { ch, step, len, p: key, v };
    let mut pats: Vec<Pattern> = vec![];
    let pattern = |pats: &mut Vec<Pattern>, name: &str, color: &str, bars: f64, swing: f32, notes: Vec<Note>| -> Id {
        let id = pats.len() as Id + 1;
        let mut pt = Pattern { id, name: name.into(), color: color.into(), len: bars * 4.0, min_len: bars * 4.0, notes, swing };
        pt.fit_len();
        pats.push(pt);
        id
    };
    let p_kick = pattern(&mut pats, "Kick", TC[0], 1.0, 0.0, [0., 4., 8., 12.].iter().map(|&s| n(kick, s, 1.0, None, 1.0)).collect());
    let p_clap = pattern(&mut pats, "Clap", TC[8], 1.0, 0.0, [4., 12.].iter().map(|&s| n(clap, s, 1.0, None, 0.9)).collect());
    // sixteenths with an accent pattern; the open hat on every off-beat
    let p_hats = pattern(&mut pats, "Hats", TC[6], 1.0, 0.3, {
        let mut v: Vec<Note> = (0..16).filter(|s| s % 4 != 2).map(|s| n(hat, s as f32, 1.0, None, [0.55, 0.3, 0.0, 0.38][s % 4])).collect();
        v.extend([2., 6., 10., 14.].iter().map(|&s| n(open_hat, s, 1.0, None, 0.8)));
        v
    });
    // the conga is one sample played at three keys
    let p_perc = pattern(&mut pats, "Perc", TC[4], 2.0, 0.3, {
        let mut v: Vec<Note> = (0..32).map(|s| n(shaker, s as f32, 1.0, None, if s % 2 == 0 { 0.5 } else { 0.28 })).collect();
        v.extend([(3., 60, 0.8), (6., 57, 0.7), (10., 60, 0.6), (13., 55, 0.75), (19., 60, 0.8), (22., 57, 0.7), (25., 62, 0.6), (27., 60, 0.5), (30., 55, 0.8)].iter().map(|&(s, k, vel)| n(conga, s, 1.0, Some(k), vel)));
        v
    });
    // Am – F – C – G, off-beat eighths with a pick-up; the last bar walks up
    let p_bass = pattern(&mut pats, "Bass", TC[5], 4.0, 0.0, {
        let mut v = vec![];
        for (bar, root) in [33u8, 29, 36, 31].iter().enumerate() {
            let at = bar as f32 * 16.0;
            for (s, len, up) in [(2., 2., 0), (6., 2., 0), (10., 2., 0), (13., 1., 12), (14., 2., 0)] {
                v.push(n(bass, at + s, len, Some(root + up), if up > 0 { 0.7 } else { 0.95 }));
            }
        }
        v
    });
    let chords: [[u8; 4]; 4] = [[57, 60, 64, 67], [53, 57, 60, 64], [55, 60, 64, 67], [55, 59, 62, 66]];
    let p_pad = pattern(&mut pats, "Pad", TC[3], 4.0, 0.0, chords.iter().enumerate().flat_map(|(bar, ch)| ch.iter().map(move |&k| n(pad, bar as f32 * 16.0, 16.0, Some(k), 0.8))).collect());
    let p_stabs = pattern(&mut pats, "Stabs", TC[2], 4.0, 0.0, {
        let mut v = vec![];
        for (bar, ch) in chords.iter().enumerate() {
            for (s, vel) in [(0., 0.9), (3., 0.7), (6., 0.8), (11., 0.6)] {
                v.extend(ch[..3].iter().map(|&k| n(stab, bar as f32 * 16.0 + s, 2.0, Some(k + 12), vel)));
            }
        }
        v
    });
    let p_marimba = pattern(&mut pats, "Marimba", TC[7], 2.0, 0.3, [(0., 57, 0.9), (3., 60, 0.7), (6., 64, 0.85), (8., 62, 0.6), (11., 60, 0.7), (14., 57, 0.8), (16., 55, 0.9), (19., 57, 0.7), (22., 60, 0.8), (24., 64, 0.9), (27., 62, 0.6), (30., 60, 0.7)].iter().map(|&(s, k, vel)| n(marimba, s, 2.0, Some(k), vel)).collect());
    let p_lead = pattern(&mut pats, "Lead", TC[1], 4.0, 0.0, [(0., 6., 76), (6., 2., 74), (8., 8., 72), (16., 6., 77), (22., 2., 76), (24., 8., 72), (32., 6., 79), (38., 2., 76), (40., 8., 72), (48., 4., 74), (52., 4., 71), (56., 8., 67)].iter().map(|&(s, l, k)| n(lead, s, l, Some(k), 0.85)).collect());
    // a snare roll that gets louder, then two toms
    let p_fill = pattern(&mut pats, "Fill", TC[8], 1.0, 0.0, {
        let mut v: Vec<Note> = [(0., 0.5), (4., 0.55), (8., 0.6), (10., 0.7), (12., 0.8), (13., 0.9)].iter().map(|&(s, vel)| n(snare, s, 1.0, None, vel)).collect();
        v.extend([(14., 62, 0.9), (15., 58, 1.0)].iter().map(|&(s, k, vel)| n(tom, s, 1.0, Some(k), vel)));
        v
    });
    let p_riser = pattern(&mut pats, "Riser", TC[9], 1.0, 0.0, vec![n(riser, 0.0, 16.0, None, 1.0)]);
    p.patterns = pats;

    // ---- the arrangement (beats; four to a bar)
    //  0  intro: hats, percussion, the pad opening up; bass from bar 5
    // 32  drop: everything; the marimba joins at bar 17
    // 96  break: pad and marimba in a growing reverb
    // 112 second drop, with the lead
    // 144 outro
    let mut clips: Vec<Clip> = vec![];
    let put = |clips: &mut Vec<Clip>, kind: ClipKind, track: u32, start: f64, len: f64, source: Id| -> usize {
        clips.push(Clip { id: clips.len() as Id + 1, track, start, len, offset: 0.0, kind, source, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None });
        clips.len() - 1
    };
    for (name, pat, spans) in [
        ("Kick", p_kick, &[(32.0, 60.0), (112.0, 32.0)][..]),
        ("Clap", p_clap, &[(32.0, 60.0), (112.0, 28.0)][..]),
        ("Clap", p_fill, &[(28.0, 4.0), (92.0, 4.0), (140.0, 4.0)][..]),
        ("Hats", p_hats, &[(0.0, 96.0), (112.0, 40.0)][..]),
        ("Perc", p_perc, &[(8.0, 20.0), (32.0, 64.0), (112.0, 32.0)][..]),
        ("Bass", p_bass, &[(16.0, 80.0), (112.0, 32.0)][..]),
        ("Pad", p_pad, &[(0.0, 160.0)][..]),
        ("Stabs", p_stabs, &[(32.0, 64.0), (112.0, 32.0)][..]),
        ("Marimba", p_marimba, &[(64.0, 48.0), (128.0, 24.0)][..]),
        ("Lead", p_lead, &[(112.0, 32.0)][..]),
        ("FX", p_riser, &[(28.0, 4.0), (108.0, 4.0)][..]),
    ] {
        for &(start, len) in spans {
            put(&mut clips, ClipKind::Pat, track(name), start, len, pat);
        }
    }
    // the crash the riser is cut from, as audio on the playlist
    let crash = crate::factory::source(&mut p, "crash-studio.flac").expect("a standard-library file");
    for start in [32.0, 112.0] {
        let i = put(&mut clips, ClipKind::Audio, track("FX"), start, 8.0, crash);
        clips[i].gain = -6.0;
        clips[i].fade_out = 3.0;
    }
    let triangle = crate::factory::source(&mut p, "triangle.flac").expect("a standard-library file");
    let i = put(&mut clips, ClipKind::Audio, track("FX"), 96.0, 6.0, triangle);
    clips[i].gain = -9.0;
    clips[i].pan = 0.4;

    // ---- the mixer
    let mut fx_id = 900;
    let mut fx = |kind: FxKind, preset: usize, set: &[(&str, f64)]| -> Fx {
        fx_id += 1;
        let mut f = new_fx(fx_id, kind, DEMO_BPM);
        load_preset(&mut f, preset, DEMO_BPM);
        for (k, v) in set {
            f.params.insert((*k).to_owned(), *v);
        }
        f
    };
    let drum_comp = fx(FxKind::Comp, 2, &[]);
    let bass_eq = fx(FxKind::Eq, 3, &[]);
    let bass_sat = fx(FxKind::Sat, 5, &[]);
    let pad_eq = fx(FxKind::Eq, 4, &[]);
    let pad_verb = fx(FxKind::Reverb, 2, &[("mix", 22.0)]);
    let stab_delay = fx(FxKind::Delay, 2, &[("mix", 18.0)]);
    let marimba_delay = fx(FxKind::Delay, 0, &[("mix", 14.0)]);
    let marimba_verb = fx(FxKind::Reverb, 1, &[("mix", 16.0)]);
    let lead_delay = fx(FxKind::Delay, 1, &[]);
    let lead_verb = fx(FxKind::Reverb, 0, &[("mix", 18.0)]);
    let fx_verb = fx(FxKind::Reverb, 2, &[("mix", 30.0)]);
    let mut vocal_eq = fx(FxKind::Eq, 1, &[]);
    vocal_eq.id = 990;
    let (vocal_comp, vocal_verb) = (fx(FxKind::Comp, 1, &[]), fx(FxKind::Reverb, 0, &[]));
    let marimba_verb_id = marimba_verb.id;
    let chain = |p: &mut Project, name: &str, vol: f32, fx: Vec<Fx>| {
        let id = insert(name);
        let m = p.inserts.iter_mut().find(|m| m.id == id).expect("a demo insert");
        m.vol = vol;
        m.fx = fx;
    };
    // levels leave the master limiter almost nothing to do (measured: `tests/sampler.rs`)
    chain(&mut p, "Drums", fader(-6.5), vec![drum_comp]);
    chain(&mut p, "Bass", fader(-5.5), vec![bass_eq, bass_sat]);
    chain(&mut p, "Pad", VOL_UNITY, vec![pad_eq, pad_verb]);
    chain(&mut p, "Stabs", VOL_UNITY, vec![stab_delay]);
    chain(&mut p, "Marimba", fader(-4.5), vec![marimba_delay, marimba_verb]);
    chain(&mut p, "Lead", VOL_UNITY, vec![lead_delay, lead_verb]);
    chain(&mut p, "FX", fader(-1.0), vec![fx_verb]);
    chain(&mut p, "Vocal", VOL_UNITY, vec![vocal_eq, vocal_comp, vocal_verb]);

    // ---- automation clips
    let auto = |p: &mut Project, clips: &mut Vec<Clip>, name: &str, target: AutoTarget, start: f64, points: &[(f64, f32)]| {
        let id = p.alloc_id();
        let len = points.last().map(|q| q.0).unwrap_or(4.0);
        p.automations.push(Automation { id, name: name.into(), color: TC[9].into(), target, points: points.iter().map(|&(t, v)| AutoPoint { t, v }).collect() });
        put(clips, ClipKind::Auto, track("Automation"), start, len, id);
    };
    // the pad's filter opens through the intro, closes in the break and opens again
    auto(&mut p, &mut clips, "Pad cutoff", AutoTarget::Synth { chan_id: pad, param: "flt_cutoff".into() }, 0.0, &[(0.0, 0.42), (28.0, 0.62), (32.0, 0.8)]);
    auto(&mut p, &mut clips, "Marimba reverb", AutoTarget::Mix { fx_id: marimba_verb_id }, 96.0, &[(0.0, 0.16), (12.0, 0.6), (16.0, 0.16)]);
    auto(&mut p, &mut clips, "Pad fade-out", AutoTarget::InsertVol { id: insert("Pad") }, 144.0, &[(0.0, VOL_UNITY), (16.0, 0.0)]);
    p.clips = clips;
    p
}
