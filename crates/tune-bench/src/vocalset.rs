//! Annotated VocalSet (Zenodo 7061507, CC BY 4.0): real singers, 14 techniques, with
//! hand-checked F0 and note annotations. Three modes:
//! * `self` — the take is the recording detuned by PSOLA (±25…400 ct per note, some with
//!   drift), the reference is the untouched recording; the target is its annotated F0.
//! * `cross` — the take (detuned the same way) against another singer performing the
//!   same exercise (other timing, often another octave); scored per note against the
//!   reference singer's annotated note pitches.
//! * `mismatch` — a vowel exercise against a song excerpt: must be left alone.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wave_engine::pitch::{analyze_with, F0Estimator};
use wave_engine::reference::PitchTarget;
use wave_engine::sources::{mono_of, Audio};
use wave_engine::{ClipTune, RefTune};

use crate::eval::{self, Case, Outcome};
use crate::synth::LEVELS;

const RATE: f32 = 200.0;

pub struct Item {
    pub stem: String,
    pub audio: PathBuf,
    /// (seconds, Hz; 0 = unvoiced)
    pub f0: Vec<(f64, f32)>,
    /// Sung notes: (start, end) seconds.
    pub notes: Vec<(f64, f64)>,
}

impl Item {
    pub fn singer(&self) -> &str {
        self.stem.split('_').next().unwrap_or("")
    }
    /// The exercise without the singer: `arpeggios_belt_c_a`.
    pub fn exercise(&self) -> &str {
        self.stem.split_once('_').map(|x| x.1).unwrap_or("")
    }
    pub fn is_excerpt(&self) -> bool {
        self.audio.components().any(|c| c.as_os_str() == "excerpts")
    }
    /// Annotated pitch at the analysis rate (MIDI; NaN = unvoiced) over `frames`.
    pub fn curve(&self, frames: usize) -> Vec<f32> {
        if self.f0.len() < 2 {
            return vec![f32::NAN; frames];
        }
        let hop = self.f0[1].0 - self.f0[0].0;
        (0..frames)
            .map(|f| {
                let x = (f as f64 / RATE as f64 - self.f0[0].0) / hop;
                let i = x.floor();
                if i < 0.0 || i as usize + 1 >= self.f0.len() {
                    return f32::NAN;
                }
                let (a, b) = (self.f0[i as usize].1, self.f0[i as usize + 1].1);
                if a <= 0.0 || b <= 0.0 {
                    return f32::NAN;
                }
                let hz = a + (b - a) * (x - i) as f32;
                69.0 + 12.0 * (hz / 440.0).log2()
            })
            .collect()
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn read_f0(path: &Path) -> Option<Vec<(f64, f32)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let rows: Vec<(f64, f32)> = text
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut c = l.split(',');
            let t = c.next()?.trim().parse().ok()?;
            let f = c.next()?.trim().parse().ok()?;
            Some((t, f))
        })
        .collect();
    (rows.len() > 1).then_some(rows)
}

fn read_notes(path: &Path) -> Option<Vec<(f64, f64)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines().skip_while(|l| !l.trim_start().starts_with("Sequence"));
    lines.next()?;
    let notes: Vec<(f64, f64)> = lines
        .filter_map(|l| {
            let c: Vec<&str> = l.split(',').map(str::trim).collect();
            (c.get(4) == Some(&"Sound")).then_some(())?;
            Some((c.get(1)?.parse().ok()?, c.get(2)?.parse().ok()?))
        })
        .filter(|(a, b)| b > a)
        .collect();
    (!notes.is_empty()).then_some(notes)
}

/// Every annotated recording under `root` (the folder holding `Annotated VocalSet`
/// and `audio`), sorted by name.
pub fn index(root: &Path) -> Result<Vec<Item>, String> {
    let ann = root.join("Annotated VocalSet");
    let mut wavs = vec![];
    walk(&root.join("audio"), &mut wavs);
    let by_stem: HashMap<String, PathBuf> = wavs
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")))
        .filter_map(|p| Some((p.file_stem()?.to_str()?.to_lowercase(), p)))
        .collect();
    if by_stem.is_empty() {
        return Err(format!("no .wav files under {}", root.join("audio").display()));
    }
    let mut items = vec![];
    for k in 1..=4 {
        let raw = ann.join(format!("raw {k}")).join("csv");
        let mut files = vec![];
        walk(&raw, &mut files);
        for f in files.into_iter().filter(|p| p.extension().is_some_and(|e| e == "csv")) {
            let Some(stem) = f.file_stem().and_then(|s| s.to_str()).map(str::to_lowercase) else {
                continue;
            };
            let technique = f.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()).unwrap_or("").to_string();
            let audio = by_stem.get(&stem).or_else(|| by_stem.get(&stem.replace("vibrado", "vibrato"))).cloned();
            let ext = ann.join(format!("extended {k}")).join("with file header").join(&technique).join(f.file_name().unwrap_or_default());
            if let (Some(audio), Some(f0), Some(notes)) = (audio, read_f0(&f), read_notes(&ext)) {
                items.push(Item { stem, audio, f0, notes });
            }
        }
    }
    items.sort_by(|a, b| a.stem.cmp(&b.stem));
    items.dedup_by(|a, b| a.stem == b.stem);
    if items.is_empty() {
        return Err(format!("no annotated recordings found under {}", ann.display()));
    }
    Ok(items)
}

/// Up to `n` items spread evenly over the sorted list (so every technique shows up).
pub fn spread<T>(v: &[T], n: usize) -> Vec<&T> {
    if v.len() <= n {
        return v.iter().collect();
    }
    (0..n).map(|i| &v[i * v.len() / n]).collect()
}

/// Per-frame error in cents put onto `audio`: note `k` off by `detune[k]`, drifting
/// by `drift` cents per second as it is held, gliding between notes.
fn offsets(frames: usize, notes: &[(f64, f64)], detune: &[f32], drift: f32) -> Vec<f32> {
    let at = |k: usize, t: f64| detune[k] + drift * (t - notes[k].0).max(0.0) as f32;
    (0..frames)
        .map(|f| {
            let t = f as f64 / RATE as f64;
            match notes.iter().position(|n| t < n.1) {
                None => at(notes.len() - 1, notes[notes.len() - 1].1),
                Some(k) if t >= notes[k].0 => at(k, t),
                Some(0) => detune[0],
                Some(k) => {
                    let (a, b) = (notes[k - 1].1, notes[k].0);
                    let x = ((t - a) / (b - a).max(1e-6)) as f32;
                    at(k - 1, a) + (detune[k] - at(k - 1, a)) * x
                }
            }
        })
        .collect()
}

/// The recording with `off` (cents per frame) added to its pitch by the editor's own PSOLA.
fn detuned(audio: &Audio, off: &[f32], est: Option<&dyn F0Estimator>) -> Audio {
    let a = analyze_with(&mono_of(audio), audio.sr as f32, est);
    let n = a.curve.len();
    let target_f0: Vec<f32> = (0..n).map(|f| a.curve[f] + off.get(f).copied().unwrap_or(0.0) / 100.0).collect();
    let pt = PitchTarget {
        rate: a.rate,
        start: 0,
        source_f0: a.curve.clone(),
        reference_f0: vec![f32::NAN; n],
        target_f0,
        alignment_confidence: vec![1.0; n],
        ref_time: vec![f32::NAN; n],
        octave: 0,
        notes: vec![],
        score: 1.0,
        reason: None,
    };
    wave_engine::tune_render::render(audio, &a, &ClipTune::default(), Some(&pt))
}

/// Frames within 40 ms of silence or of a note edge, or inside a fast glide/trill.
fn edges(truth: &[f32], notes: &[(f64, f64)]) -> Vec<bool> {
    let n = truth.len();
    let near_note_edge = |f: usize| {
        let t = f as f64 / RATE as f64;
        notes.iter().any(|&(a, b)| (t - a).abs() < 0.04 || (t - b).abs() < 0.04) || !notes.iter().any(|&(a, b)| t > a && t < b)
    };
    (0..n)
        .map(|f| {
            let (a, b) = (f.saturating_sub(8), (f + 8).min(n - 1));
            near_note_edge(f) || truth[a..=b].iter().any(|x| x.is_nan()) || (truth[b] - truth[a]).abs() > 0.6
        })
        .collect()
}

fn note_pitch(curve: &[f32], (a, b): (f64, f64)) -> Option<f32> {
    let hi = (((b - 0.03) * RATE as f64) as usize).min(curve.len());
    let lo = (((a + 0.05) * RATE as f64) as usize).min(hi);
    let mut v: Vec<f32> = curve[lo..hi].iter().copied().filter(|x| x.is_finite()).collect();
    if v.len() < 4 {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

fn frames_of(a: &Audio) -> usize {
    (a.seconds() * RATE as f64) as usize + 1
}

pub fn run_self(items: &[&Item], p: &RefTune, est: Option<&dyn F0Estimator>) -> Vec<Outcome> {
    let mut out = vec![];
    for (i, it) in items.iter().enumerate() {
        let Ok(audio) = Audio::load(&it.audio) else {
            continue;
        };
        let frames = frames_of(&audio);
        let truth = it.curve(frames);
        let level = LEVELS[i % LEVELS.len()];
        let drift = if i % 6 == 5 { 30.0 } else { 0.0 };
        let detune: Vec<f32> = (0..it.notes.len()).map(|k| if (k + i) % 2 == 0 { level } else { -level }).collect();
        let off = offsets(frames, &it.notes, &detune, drift);
        let take = detuned(&audio, &off, est);
        let sung: Vec<f32> = truth.iter().zip(&off).map(|(t, o)| t + o / 100.0).collect();
        let notes: Vec<(f64, f64, f32)> = it.notes.iter().filter_map(|&n| Some((n.0, n.1, note_pitch(&truth, n)?))).collect();
        let c = Case {
            name: it.stem.clone(),
            group: if drift > 0.0 { format!("±{level:.0} ct + drift") } else { format!("±{level:.0} ct") },
            take: &take,
            reference: &audio,
            edge: edges(&truth, &it.notes),
            truth,
            sung,
            notes,
            onsets: it.notes.iter().map(|n| (n.0, n.0)).collect(),
            expect_rejection: false,
        };
        let (o, _) = eval::run(&c, p, est);
        eprintln!(
            "  self  {:<34} {:<14} score {:.2} frame median {:5.1} ct (was {:6.1}) {}",
            o.name,
            o.group,
            o.score,
            eval::quantile(&o.frames_after, 0.5),
            eval::quantile(&o.frames_before, 0.5),
            o.reason.as_deref().unwrap_or("")
        );
        out.push(o);
    }
    out
}

/// Pairs of the same exercise sung by two singers with the same number of notes.
pub fn cross_pairs(items: &[Item]) -> Vec<(&Item, &Item)> {
    let mut pairs = vec![];
    for (i, a) in items.iter().enumerate() {
        if a.is_excerpt() {
            continue;
        }
        if let Some(b) =
            items.iter().skip(i + 1).find(|b| b.exercise() == a.exercise() && b.singer() != a.singer() && b.notes.len() == a.notes.len())
        {
            pairs.push((a, b));
        }
    }
    pairs
}

pub fn run_cross(pairs: &[&(&Item, &Item)], p: &RefTune, est: Option<&dyn F0Estimator>) -> Vec<Outcome> {
    let mut out = vec![];
    for (i, (ti, ri)) in pairs.iter().enumerate() {
        let (Ok(orig), Ok(reference)) = (Audio::load(&ti.audio), Audio::load(&ri.audio)) else {
            continue;
        };
        let frames = frames_of(&orig);
        let truth_take = ti.curve(frames);
        let truth_ref = ri.curve(frames_of(&reference));
        let level = LEVELS[i % LEVELS.len()];
        let detune: Vec<f32> = (0..ti.notes.len()).map(|k| if (k + i) % 2 == 0 { level } else { -level }).collect();
        let off = offsets(frames, &ti.notes, &detune, 0.0);
        let take = detuned(&orig, &off, est);
        let sung: Vec<f32> = truth_take.iter().zip(&off).map(|(t, o)| t + o / 100.0).collect();
        // the take's note k should land on the reference singer's note k, in the take's octave
        let notes: Vec<(f64, f64, f32)> = ti
            .notes
            .iter()
            .zip(&ri.notes)
            .filter_map(|(&tn, &rn)| {
                let (own, theirs) = (note_pitch(&truth_take, tn)?, note_pitch(&truth_ref, rn)?);
                Some((tn.0, tn.1, theirs + 12.0 * ((own - theirs) / 12.0).round()))
            })
            .collect();
        let c = Case {
            name: format!("{} ← {}", ti.stem, ri.stem),
            group: format!("±{level:.0} ct"),
            take: &take,
            reference: &reference,
            truth: vec![],
            edge: vec![],
            sung,
            notes,
            onsets: ti.notes.iter().zip(&ri.notes).map(|(a, b)| (a.0, b.0)).collect(),
            expect_rejection: false,
        };
        let (o, _) = eval::run(&c, p, est);
        let mae = |v: &[f32]| v.iter().map(|x| x.abs()).sum::<f32>() / v.len().max(1) as f32;
        eprintln!(
            "  cross {:<52} score {:.2} oct {:+3} note MAE {:5.1} ct (was {:6.1}) {}",
            o.name,
            o.score,
            o.octave,
            mae(&o.notes_after),
            mae(&o.notes_before),
            o.reason.as_deref().unwrap_or("")
        );
        out.push(o);
    }
    out
}

pub fn run_mismatch(items: &[Item], n: usize, p: &RefTune, est: Option<&dyn F0Estimator>) -> Vec<Outcome> {
    let excerpts: Vec<&Item> = items.iter().filter(|i| i.is_excerpt()).collect();
    let exercises: Vec<&Item> = items.iter().filter(|i| !i.is_excerpt()).collect();
    if excerpts.is_empty() || exercises.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    for (i, ti) in spread(&exercises, n).into_iter().enumerate() {
        let ri = excerpts[i % excerpts.len()];
        let (Ok(take), Ok(reference)) = (Audio::load(&ti.audio), Audio::load(&ri.audio)) else {
            continue;
        };
        let frames = frames_of(&take);
        let c = Case {
            name: format!("{} ← {}", ti.stem, ri.stem),
            group: "mismatch".into(),
            take: &take,
            reference: &reference,
            truth: vec![],
            edge: vec![],
            sung: vec![f32::NAN; frames],
            notes: vec![],
            onsets: vec![],
            expect_rejection: true,
        };
        let (o, _) = eval::run(&c, p, est);
        eprintln!("  miss  {:<52} score {:.2} {}", o.name, o.score, if o.corrected { "CORRECTED (wrong)" } else { "held back" });
        out.push(o);
    }
    out
}
