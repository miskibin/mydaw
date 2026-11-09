//! Deterministic synthetic projects of several sizes, with real WAV files, for benchmarking.
//!
//! `build` writes the audio into `<dir>/audio` and returns the [`Project`]; `generate`
//! also saves it as a `.wave` file (relative audio paths, like a real song) plus a plain
//! `project.json` that the UI mock backend can load.

use std::f32::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wave_engine::catalog::{new_fx, params};
use wave_engine::sources::write_wav;
use wave_engine::*;

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1) * 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// 0..1
    pub fn f(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.f()
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub name: &'static str,
    pub tracks: usize,
    pub audio_clips: usize,
    pub pat_clips: usize,
    pub patterns: usize,
    pub notes: usize,
    pub sources: usize,
    pub src_secs: (f64, f64),
    pub beats: f64,
    pub bpm: f64,
    pub automations: usize,
    pub auto_points: usize,
    pub comps: usize,
    pub tuned_clips: usize,
    /// Tracks with the heavy vocal chain (denoise, tune, voice, ...).
    pub vocal_tracks: usize,
}

pub fn specs() -> Vec<Spec> {
    vec![
        Spec {
            name: "small",
            tracks: 8,
            audio_clips: 60,
            pat_clips: 12,
            patterns: 6,
            notes: 64,
            sources: 8,
            src_secs: (4.0, 12.0),
            beats: 256.0,
            bpm: 128.0,
            automations: 4,
            auto_points: 40,
            comps: 1,
            tuned_clips: 1,
            vocal_tracks: 1,
        },
        Spec {
            name: "medium",
            tracks: 32,
            audio_clips: 420,
            pat_clips: 60,
            patterns: 14,
            notes: 160,
            sources: 20,
            src_secs: (5.0, 18.0),
            beats: 384.0,
            bpm: 90.0,
            automations: 16,
            auto_points: 100,
            comps: 2,
            tuned_clips: 2,
            vocal_tracks: 3,
        },
        // 512 beats is the arrangement limit (TOTAL_BEATS); at 52 bpm that is a 9.8 minute song
        Spec {
            name: "large",
            tracks: 64,
            audio_clips: 1200,
            pat_clips: 160,
            patterns: 24,
            notes: 256,
            sources: 40,
            src_secs: (6.0, 24.0),
            beats: 512.0,
            bpm: 52.0,
            automations: 40,
            auto_points: 200,
            comps: 3,
            tuned_clips: 4,
            vocal_tracks: 6,
        },
        Spec {
            name: "huge",
            tracks: 128,
            audio_clips: 3000,
            pat_clips: 400,
            patterns: 48,
            notes: 400,
            sources: 80,
            src_secs: (6.0, 24.0),
            beats: 512.0,
            bpm: 52.0,
            automations: 100,
            auto_points: 300,
            comps: 4,
            tuned_clips: 8,
            vocal_tracks: 12,
        },
    ]
}

pub fn spec(name: &str) -> Option<Spec> {
    specs().into_iter().find(|s| s.name == name)
}

/// Style of source `i`: 0 = voice-like, 1 = pad, 2 = drum loop.
pub fn style(i: usize) -> usize {
    i % 3
}

fn synth(style: usize, secs: f64, sr: u32, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let n = (secs * sr as f64) as usize;
    let mut out = vec![0f32; n * 2];
    let srf = sr as f32;
    match style {
        0 => {
            // a sung phrase: notes glide, vibrato, formants, breath noise, silences between phrases
            let mut i = 0;
            let mut phase = 0f32;
            while i < n {
                let len = (rng.range(0.25, 0.9) as f32 * srf) as usize;
                let f0 = 130.0 * 2f32.powf(rng.below(14) as f32 / 12.0);
                let rest = rng.f() < 0.18;
                for k in 0..len.min(n - i) {
                    let t = k as f32 / srf;
                    let env = if rest {
                        0.0
                    } else {
                        (t * 40.0).min(1.0) * (((len - k) as f32 / srf) * 30.0).min(1.0)
                    };
                    let vib = 1.0 + 0.012 * (TAU * 5.5 * t).sin() * (t * 3.0).min(1.0);
                    phase += TAU * f0 * vib / srf;
                    if phase > TAU {
                        phase -= TAU;
                    }
                    let mut s = 0.0;
                    for h in 1..=14 {
                        let hf = f0 * h as f32;
                        let form = (-((hf - 700.0) / 500.0).powi(2)).exp()
                            + 0.6 * (-((hf - 2400.0) / 700.0).powi(2)).exp();
                        s += (phase * h as f32).sin() * form / h as f32;
                    }
                    let noise = (rng.f() as f32 - 0.5) * 0.02;
                    let v = (s * 0.35 + noise) * env * 0.8 + (rng.f() as f32 - 0.5) * 0.004;
                    out[(i + k) * 2] = v;
                    out[(i + k) * 2 + 1] = v * 0.97;
                }
                i += len;
            }
        }
        1 => {
            let notes = [0.0, 3.0, 7.0, 10.0];
            for (c, o) in notes.iter().enumerate() {
                let f = 110.0 * 2f32.powf((*o + (seed % 5) as f32) / 12.0);
                for i in 0..n {
                    let t = i as f32 / srf;
                    let saw = ((t * f * (1.0 + c as f32 * 0.003)) % 1.0) * 2.0 - 1.0;
                    let env = (t * 2.0).min(1.0) * ((secs as f32 - t) * 2.0).clamp(0.0, 1.0);
                    let v = saw * 0.12 * env;
                    out[i * 2] += v * (1.0 - c as f32 * 0.2);
                    out[i * 2 + 1] += v * (0.6 + c as f32 * 0.2);
                }
            }
        }
        _ => {
            let beat = (srf * 60.0 / 120.0) as usize;
            for b in 0..(n / beat + 1) {
                let at = b * beat;
                for k in 0..(beat / 2).min(n.saturating_sub(at)) {
                    let t = k as f32 / srf;
                    let kick =
                        (TAU * (60.0 + 90.0 * (-t * 30.0).exp()) * t).sin() * (-t * 9.0).exp();
                    let hat = if b % 2 == 1 {
                        (rng.f() as f32 - 0.5) * (-t * 40.0).exp() * 0.5
                    } else {
                        0.0
                    };
                    let v = kick * 0.7 + hat;
                    out[(at + k) * 2] += v;
                    out[(at + k) * 2 + 1] += v;
                }
            }
        }
    }
    out
}

fn color(i: usize) -> String {
    [
        "#4f8ef7", "#5aa9e6", "#69c3d6", "#47b8a8", "#5fbf8a", "#7c8cf8", "#e0a44a", "#9a8cf0",
        "#d9678f", "#8b93a1",
    ][i % 10]
        .into()
}

const LIGHT: [&[FxKind]; 4] = [
    &[FxKind::Eq, FxKind::Comp],
    &[FxKind::Eq, FxKind::Comp, FxKind::Sat],
    &[FxKind::Eq, FxKind::Comp, FxKind::Delay, FxKind::Reverb],
    &[FxKind::Eq, FxKind::Deess, FxKind::Comp, FxKind::Limit],
];
const VOCAL: &[FxKind] = &[
    FxKind::Denoise,
    FxKind::Tune,
    FxKind::Voice,
    FxKind::Eq,
    FxKind::Deess,
    FxKind::Comp,
    FxKind::Fet,
    FxKind::Sat,
    FxKind::Reverb,
];

fn new_clip(id: Id, track: u32, start: f64, len: f64, kind: ClipKind, source: Id) -> Clip {
    Clip {
        id,
        track,
        start,
        len,
        offset: 0.0,
        kind,
        source,
        muted: false,
        gain: 0.0,
        fade_in: 0.0,
        fade_out: 0.0,
        pan: 0.0,
        norm: false,
        rev: false,
        tune: None,
    }
}

/// Builds the project and writes its audio under `<dir>/audio`.
pub fn build(spec: &Spec, dir: &Path) -> Project {
    let mut rng = Rng::new(7 + spec.tracks as u64);
    let audio_dir = dir.join("audio");
    std::fs::create_dir_all(&audio_dir).unwrap();
    let mut p = Project::empty();
    p.bpm = spec.bpm;
    p.tracks.clear();
    p.inserts.truncate(1); // master
    let spb = 60.0 / spec.bpm;

    // ---- sources
    let mut src_style = vec![];
    for i in 0..spec.sources {
        let st = style(i);
        let secs = rng.range(spec.src_secs.0, spec.src_secs.1);
        let sr = if i % 4 == 3 { 44100 } else { 48000 };
        let path = audio_dir.join(format!("src{i:03}.wav"));
        if !path.exists() {
            write_wav(&path, &synth(st, secs, sr, 100 + i as u64), sr).unwrap();
        }
        let id = p.alloc_id();
        p.sources.push(Source {
            id,
            name: format!("{} {i}", ["Vocal", "Pad", "Drums"][st]),
            color: color(i),
            path: path.display().to_string(),
            dur: secs,
            sr,
            ch: 2,
            peak_db: -6.0,
        });
        src_style.push(st);
    }
    let vocals: Vec<usize> = (0..spec.sources).filter(|i| style(*i) == 0).collect();
    let drums: Vec<usize> = (0..spec.sources).filter(|i| style(*i) == 2).collect();

    // ---- sampler channels
    for (k, &di) in drums.iter().take(3).enumerate() {
        let id = p.alloc_id();
        let sid = p.sources[di].id;
        p.channels.push(Channel {
            id,
            name: format!("Sampler {k}"),
            color: color(k + 3),
            src: ChannelSrc::Sample { source: sid },
            vol: VOL_UNITY,
            pan: 0.0,
            mute: false,
            solo: false,
            insert: None,
            root: SAMPLE_ROOT,
        });
    }

    // ---- mixer: master + one insert per track
    for fx in [FxKind::Comp, FxKind::Eq] {
        let id = p.alloc_id();
        p.inserts[0].fx.insert(0, new_fx(id, fx, spec.bpm)); // before the master limiter
    }
    for t in 0..spec.tracks {
        let ins_id = p.alloc_id();
        let tid = p.alloc_id();
        let vocal = t < spec.vocal_tracks;
        let kinds: &[FxKind] = if vocal { VOCAL } else { LIGHT[t % 4] };
        let mut fx = vec![];
        for k in kinds {
            let id = p.alloc_id();
            fx.push(new_fx(id, *k, spec.bpm));
        }
        p.inserts.push(Insert {
            id: ins_id,
            name: format!("Ins {t}"),
            color: color(t),
            vol: VOL_UNITY,
            pan: ((t % 7) as f32 - 3.0) / 6.0,
            width: 1.0,
            muted: false,
            solo: false,
            fx,
        });
        p.tracks.push(Track {
            id: tid,
            name: if vocal {
                format!("Vocal {t}")
            } else {
                format!("Track {t}")
            },
            color: color(t),
            muted: false,
            solo: false,
            arm: false,
            vol: VOL_UNITY * 0.9,
            h: 56.0,
            insert: ins_id,
            icon: None,
        });
    }

    // ---- patterns
    let chans: Vec<Id> = p.channels.iter().map(|c| c.id).collect();
    let pitched: Vec<Id> = p
        .channels
        .iter()
        .filter(|c| c.pitched())
        .map(|c| c.id)
        .collect();
    let mut pats = vec![];
    for i in 0..spec.patterns {
        let id = p.alloc_id();
        let len = if i % 3 == 0 { 8.0 } else { 16.0 };
        let steps = (len * 4.0) as usize;
        let mut notes = vec![];
        for _ in 0..spec.notes {
            let ch = chans[rng.below(chans.len())];
            let pitched_ch = pitched.contains(&ch);
            notes.push(Note::new(
                ch,
                rng.below(steps) as f32,
                [1.0, 1.0, 2.0, 4.0][rng.below(4)],
                if pitched_ch {
                    Some(36 + rng.below(36) as u8)
                } else {
                    None
                },
            ));
        }
        notes.sort_by(|a, b| a.step.total_cmp(&b.step));
        p.patterns.push(Pattern {
            id,
            name: format!("Pattern {i}"),
            color: color(i),
            len,
            notes,
            swing: 0.0,
        });
        pats.push(id);
    }
    // one stress pattern for the piano roll
    {
        let id = p.alloc_id();
        let ch = pitched.first().copied().unwrap_or(chans[0]);
        let mut notes: Vec<Note> = (0..2000)
            .map(|_| {
                Note::new(
                    ch,
                    rng.below(1024) as f32,
                    [1.0, 2.0, 4.0][rng.below(3)],
                    Some(24 + rng.below(60) as u8),
                )
            })
            .collect();
        notes.sort_by(|a, b| a.step.total_cmp(&b.step));
        p.patterns.push(Pattern {
            id,
            name: "Piano roll stress (2000 notes)".into(),
            color: color(9),
            len: 256.0,
            notes,
            swing: 0.0,
        });
    }

    // ---- clips
    let cid = |p: &mut Project| p.alloc_id();
    for i in 0..spec.audio_clips {
        let track = if i < spec.tuned_clips {
            i % spec.vocal_tracks.max(1)
        } else {
            rng.below(spec.tracks)
        };
        let si = if track < spec.vocal_tracks {
            vocals[rng.below(vocals.len())]
        } else {
            rng.below(spec.sources)
        };
        let s = &p.sources[si];
        let len = (s.dur / spb).min(spec.beats / 4.0).max(0.5);
        let start = if i < spec.tuned_clips {
            4.0 + 20.0 * i as f64
        } else {
            rng.range(0.0, (spec.beats - len).max(0.0))
        };
        let src = s.id;
        let id = cid(&mut p);
        let mut c = new_clip(
            id,
            track as u32,
            (start * 4.0).round() / 4.0,
            len,
            ClipKind::Audio,
            src,
        );
        c.gain = rng.range(-6.0, 2.0) as f32;
        if i % 5 == 0 {
            c.fade_in = 0.02;
            c.fade_out = 0.05;
        }
        if i % 23 == 0 {
            c.norm = true;
        }
        if i % 41 == 0 {
            c.rev = true;
        }
        if i < spec.tuned_clips && style(si) == 0 {
            c.tune = Some(ClipTune {
                notes: (0..3)
                    .map(|k| NoteEdit {
                        start: 0.4 + k as f64 * 0.7,
                        end: 1.0 + k as f64 * 0.7,
                        shift: 40.0 * (k as f32 - 1.0),
                        drift: 0.5,
                        formant: 0.0,
                        gain: 0.0,
                    })
                    .collect(),
                pencil: vec![],
                reference: None,
            });
        }
        p.clips.push(c);
    }
    for _ in 0..spec.pat_clips {
        let track = rng.below(spec.tracks.max(1));
        let pat = pats[rng.below(pats.len())];
        let plen = p.patterns.iter().find(|x| x.id == pat).unwrap().len;
        let start = (rng.range(0.0, spec.beats - plen) / 4.0).floor() * 4.0;
        let id = cid(&mut p);
        p.clips
            .push(new_clip(id, track as u32, start, plen, ClipKind::Pat, pat));
    }

    // ---- automation
    let mut autos = vec![];
    for i in 0..spec.automations {
        let id = p.alloc_id();
        let ins = &p.inserts[1 + rng.below(spec.tracks)];
        let target = if i % 2 == 0 {
            AutoTarget::InsertVol { id: ins.id }
        } else {
            let f = &ins.fx[rng.below(ins.fx.len())];
            match params(f.kind).first() {
                Some(d) => AutoTarget::Fx {
                    fx_id: f.id,
                    param: d.id.to_string(),
                },
                None => AutoTarget::InsertPan { id: ins.id },
            }
        };
        let span = 64.0;
        let points: Vec<AutoPoint> = (0..spec.auto_points)
            .map(|k| AutoPoint {
                t: span * k as f64 / (spec.auto_points - 1).max(1) as f64,
                v: (0.5 + 0.4 * (k as f32 * 0.37 + i as f32).sin()).clamp(0.0, 1.0),
            })
            .collect();
        p.automations.push(Automation {
            id,
            name: format!("Auto {i}"),
            color: color(i),
            target,
            points,
        });
        autos.push((id, span));
    }
    for (k, (aid, span)) in autos.iter().enumerate() {
        let id = cid(&mut p);
        let track = (k * 3) % spec.tracks;
        p.clips.push(new_clip(
            id,
            track as u32,
            (k % 8) as f64 * 64.0,
            *span,
            ClipKind::Auto,
            *aid,
        ));
    }

    // ---- comps (takes of a vocal source, comp clip on a vocal track)
    for k in 0..spec.comps {
        let id = p.alloc_id();
        let mut takes = vec![];
        for t in 0..4 {
            let tid = p.alloc_id();
            let si = vocals[(k * 4 + t) % vocals.len()];
            let s = &p.sources[si];
            takes.push(Take {
                id: tid,
                name: format!("Take {}", t + 1),
                source: s.id,
                start: 0.0,
                len: s.dur / spb,
                offset: 0.0,
                color: None,
            });
        }
        let end = takes.iter().map(|t| t.len).fold(f64::MAX, f64::min);
        let mut comp = Comp {
            id,
            name: format!("Comp {k}"),
            takes,
            regions: vec![],
            xfade: DEFAULT_XFADE,
        };
        let seg = end / 6.0;
        for r in 0..6 {
            let take = comp.takes[r % 4].id;
            comp.write(take, r as f64 * seg, (r + 1) as f64 * seg);
        }
        let cl = cid(&mut p);
        let track = (k % spec.vocal_tracks.max(1)) as u32;
        p.clips.push(new_clip(
            cl,
            track,
            300.0 + k as f64 * 40.0,
            end.min(32.0),
            ClipKind::Comp,
            id,
        ));
        p.comps.push(comp);
    }

    p.loop_range = Some(LoopRange { a: 16.0, b: 48.0 });
    p
}

pub struct Generated {
    pub dir: PathBuf,
    pub wave: PathBuf,
    pub project: Project,
}

/// Builds, saves `<dir>/<name>.wave` and writes `<dir>/project.json` (UI fixture).
pub fn generate(spec: &Spec, root: &Path) -> Generated {
    let dir = root.join(spec.name);
    let project = build(spec, &dir);
    let e = Engine::new(project.clone(), Arc::new(NoPlugins));
    let wave = dir.join(format!("{}.wave", spec.name));
    e.save(&wave).expect("save fixture");
    let ui = serde_json::to_vec(&project).unwrap();
    std::fs::write(dir.join("project.json"), ui).unwrap();
    Generated { dir, wave, project }
}
