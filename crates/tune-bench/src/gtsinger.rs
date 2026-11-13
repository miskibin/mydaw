//! GTSinger (CC BY-NC-SA 4.0; `scripts/fetch_gtsinger.py` fetches the subset): real singers,
//! each phrase sung plainly (`Control_Group`) and with a technique (`Vibrato_Group`,
//! `Glissando_Group`), with word/note timing and the score's MIDI notes per segment, technique
//! flags per note. No F0 annotation: where a frame truth is needed it is the analysis of the
//! unmodified recording (reported as `original-analysis`), and note intentions come from the
//! score and the technique flags.

use std::path::{Path, PathBuf};

use serde_json::Value;
use wave_engine::pitch::{analyze_with, F0Estimator};
use wave_engine::sources::{mono_of, Audio};

use crate::agent::{Label, Note};
use crate::cases::{inject, median, plan, Owned};
use crate::vocalset::edges;

#[derive(Clone, Debug)]
pub struct GNote {
    pub start: f64,
    pub end: f64,
    pub midi: f32,
    pub word: String,
    pub glissando: bool,
}

#[derive(Clone, Debug)]
pub struct Seg {
    pub singer: String,
    pub tech: String,
    pub song: String,
    pub group: String,
    pub id: String,
    pub wav: PathBuf,
    pub notes: Vec<GNote>,
}

impl Seg {
    pub fn name(&self) -> String {
        format!("{}/{}/{}/{}/{}", self.singer, self.tech, self.song, self.group, self.id)
    }
    pub fn spans(&self) -> Vec<(f64, f64)> {
        self.notes.iter().map(|n| (n.start, n.end)).collect()
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "json") {
            out.push(p);
        }
    }
}

fn flag(v: &Value, k: usize) -> bool {
    v.get(k).and_then(|x| x.as_str().map(|s| s == "1").or(x.as_i64().map(|i| i == 1))).unwrap_or(false)
}

fn read(json: &Path) -> Option<Seg> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(json).ok()?).ok()?;
    let mut notes = vec![];
    for w in v.as_array()? {
        let word = w.get("word")?.as_str()?.to_string();
        let (ns, st, en) = (w.get("note")?.as_array()?, w.get("note_start")?.as_array()?, w.get("note_end")?.as_array()?);
        for (k, note) in ns.iter().enumerate() {
            let midi = note.as_f64()? as f32;
            let (a, b) = (st.get(k)?.as_f64()?, en.get(k)?.as_f64()?);
            if midi <= 0.0 || b <= a {
                continue;
            }
            notes.push(GNote {
                start: a,
                end: b,
                midi,
                word: word.clone(),
                glissando: flag(w.get("glissando").unwrap_or(&Value::Null), k),
            });
        }
    }
    let p: Vec<String> = json.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let n = p.len();
    if n < 5 || notes.is_empty() {
        return None;
    }
    let wav = json.with_extension("wav");
    wav.is_file().then(|| Seg {
        singer: p[n - 5].clone(),
        tech: p[n - 4].clone(),
        song: p[n - 3].clone(),
        group: p[n - 2].clone(),
        id: p[n - 1].trim_end_matches(".json").to_string(),
        wav,
        notes,
    })
}

pub fn index(root: &Path) -> Result<Vec<Seg>, String> {
    let mut files = vec![];
    walk(&root.join("English"), &mut files);
    let mut v: Vec<Seg> = files.iter().filter_map(|f| read(f)).collect();
    v.sort_by_key(|s| s.name());
    if v.is_empty() {
        return Err(format!("no GTSinger segments (wav + json) under {}", root.join("English").display()));
    }
    Ok(v)
}

/// The same phrase from another group or singer: matched note by note through the words.
fn word_pairs(a: &Seg, b: &Seg) -> Vec<(usize, usize)> {
    // longest common subsequence of words, first note of each word
    let first = |s: &Seg| -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = vec![];
        for (k, n) in s.notes.iter().enumerate() {
            if out.last().is_none_or(|l| l.0 != n.word || s.notes[l.1].end + 1e-6 < n.start - 0.3) {
                out.push((n.word.clone(), k));
            }
        }
        out
    };
    let (x, y) = (first(a), first(b));
    let (n, m) = (x.len(), y.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            d[i][j] = if x[i].0 == y[j].0 { d[i + 1][j + 1] + 1 } else { d[i + 1][j].max(d[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < n && j < m {
        if x[i].0 == y[j].0 {
            out.push((x[i].1, y[j].1));
            i += 1;
            j += 1;
        } else if d[i + 1][j] >= d[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

fn load(s: &Seg) -> Result<Audio, String> {
    Audio::load(&s.wav).map_err(|e| format!("{}: {e}", s.wav.display()))
}

/// The analysis of the unmodified recording as frame truth, and each note's median over it.
fn original_truth(a: &Audio, spans: &[(f64, f64)], est: Option<&dyn F0Estimator>) -> (Vec<f32>, Vec<Option<f32>>) {
    let an = analyze_with(&mono_of(a), a.sr as f32, est);
    let nt = spans
        .iter()
        .map(|&(s, e)| {
            let (f0, f1) = (((s + 0.05) * 200.0) as usize, (((e - 0.03) * 200.0) as usize).min(an.curve.len()));
            if f1 <= f0 + 4 {
                None
            } else {
                median(an.curve[f0..f1].to_vec())
            }
        })
        .collect();
    (an.curve, nt)
}

/// `skipped`: pairs the dataset cannot provide (the two singers' segments with one index hold
/// different lines), which are no failure of the method.
pub fn cases(
    segs: &[Seg],
    limit: usize,
    est: Option<&dyn F0Estimator>,
    failed: &mut Vec<(String, String)>,
    skipped: &mut Vec<String>,
) -> Vec<Owned> {
    let mut out = vec![];
    let find = |singer: &str, tech: &str, song: &str, group: &str, id: &str| {
        segs.iter().find(|s| s.singer == singer && s.tech == tech && s.song == song && s.group == group && s.id == id)
    };
    // technique segments with their plain twin
    let pairs: Vec<(&Seg, &Seg)> = segs
        .iter()
        .filter(|s| s.group != "Control_Group")
        .filter_map(|s| Some((s, find(&s.singer, &s.tech, &s.song, "Control_Group", &s.id)?)))
        .collect();
    for (i, (tech, plain)) in crate::vocalset::spread(&pairs, limit).into_iter().enumerate() {
        let (ta, pa) = match (load(tech), load(plain)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                failed.push((tech.name(), e));
                continue;
            }
        };
        let wp = word_pairs(tech, plain);
        // 1. natural ornamented take against the plain phrase: slides must stay, vibrato too
        let frames = (ta.seconds() * 200.0) as usize + 1;
        let notes: Vec<Note> = tech
            .notes
            .iter()
            .map(|n| Note { start: n.start, end: n.end, label: if n.glissando { Label::Intended } else { Label::Natural }, want: n.midi })
            .collect();
        out.push(Owned {
            name: format!("{} ← plain", tech.name()),
            group: format!("natural {} take, plain reference", tech.tech.to_lowercase()),
            take: Audio::from_stereo(ta.content().to_vec(), ta.sr, ta.channels),
            reference: Some(Audio::from_stereo(pa.content().to_vec(), pa.sr, pa.channels)),
            truth: vec![f32::NAN; frames],
            edge: vec![true; frames],
            sung: vec![f32::NAN; frames],
            notes,
            expect_related: Some(true),
            expect_interval: Some(0),
            pairs: wp
                .iter()
                .map(|&(a, b)| ((tech.notes[a].start, tech.notes[a].end), (plain.notes[b].start, plain.notes[b].end)))
                .collect(),
            truth_kind: "score",
        });
        // 2. the plain take with slips against the ornamented reference
        let spans = plain.spans();
        let (truth, nt) = original_truth(&pa, &spans, est);
        let p = plan("slips", spans.len(), i);
        let (take, sung, notes) = inject(&pa, &spans, &nt, &truth, &p, 0.0, est);
        out.push(Owned {
            name: format!("{} ← {}", plain.name(), tech.group),
            group: format!("slips, {} reference", tech.tech.to_lowercase()),
            take,
            reference: Some(ta),
            edge: edges(&truth, &spans),
            truth,
            sung,
            notes,
            expect_related: Some(true),
            expect_interval: Some(0),
            pairs: wp
                .iter()
                .map(|&(a, b)| ((plain.notes[b].start, plain.notes[b].end), (tech.notes[a].start, tech.notes[a].end)))
                .collect(),
            truth_kind: "original-analysis",
        });
    }
    // 3. the same song by two singers (tenor / alto): another range, a non-octave interval
    let control: Vec<&Seg> = segs.iter().filter(|s| s.group == "Control_Group").collect();
    let mut cross = vec![];
    for a in &control {
        for b in &control {
            if a.singer < b.singer && a.tech == b.tech && a.song == b.song && a.id == b.id {
                cross.push((*a, *b));
            }
        }
    }
    for (i, (x, y)) in crate::vocalset::spread(&cross, limit).into_iter().enumerate() {
        for (take_s, ref_s) in [(x, y), (y, x)] {
            let (ta, ra) = match (load(take_s), load(ref_s)) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) => {
                    failed.push((take_s.name(), e));
                    continue;
                }
            };
            let wp = word_pairs(take_s, ref_s);
            // the same index is not always the same line for both singers
            if wp.len() * 2 < take_s.notes.len().min(ref_s.notes.len()) || wp.len() < 3 {
                skipped.push(format!("{} ← {}: the two segments are different lines", take_s.name(), ref_s.singer));
                continue;
            }
            let expect = median(wp.iter().map(|&(a, b)| take_s.notes[a].midi - ref_s.notes[b].midi).collect()).map(|d| d.round() as i32);
            let spans = take_s.spans();
            let (truth, nt) = original_truth(&ta, &spans, est);
            let p = plan("slips", spans.len(), i);
            let (take, sung, notes) = inject(&ta, &spans, &nt, &truth, &p, 0.0, est);
            out.push(Owned {
                name: format!("{} ← {}", take_s.name(), ref_s.singer),
                group: "cross-singer, another key".into(),
                take,
                reference: Some(ra),
                edge: edges(&truth, &spans),
                truth,
                sung,
                notes,
                expect_related: Some(true),
                expect_interval: expect,
                pairs: wp
                    .iter()
                    .map(|&(a, b)| ((take_s.notes[a].start, take_s.notes[a].end), (ref_s.notes[b].start, ref_s.notes[b].end)))
                    .collect(),
                truth_kind: "original-analysis",
            });
        }
    }
    out
}

/// Detector + segmentation against the score: each score note's median detected pitch, in cents
/// from the score note (singers are free to drift from a score, so this bounds, not measures,
/// detector error).
pub fn score_agreement(segs: &[Seg], limit: usize, est: Option<&dyn F0Estimator>) -> Value {
    let mut errs = vec![];
    let mut missing = 0usize;
    let plain: Vec<&Seg> = segs.iter().filter(|s| s.group == "Control_Group").collect();
    for s in crate::vocalset::spread(&plain, limit) {
        let Ok(a) = load(s) else {
            missing += s.notes.len();
            continue;
        };
        let (_, nt) = original_truth(&a, &s.spans(), est);
        for (n, m) in s.notes.iter().zip(nt) {
            match m {
                Some(m) => errs.push((m - n.midi) * 100.0),
                None => missing += 1,
            }
        }
    }
    let abs: Vec<f32> = errs.iter().map(|x| x.abs()).collect();
    serde_json::json!({
        "notes": errs.len(), "unmeasured": missing,
        "within50ct": abs.iter().filter(|x| **x < 50.0).count() as f32 / abs.len().max(1) as f32,
        "medianAbsCents": crate::eval::quantile(&abs, 0.5), "p90AbsCents": crate::eval::quantile(&abs, 0.9),
        "octaveOff": abs.iter().filter(|x| **x > 600.0).count(),
    })
}
