//! The agent workflow, end to end, on real recordings: analyse the take, optionally compare it
//! with a reference, let the conservative planner (`pitch_api::suggest`) decide, apply its edit
//! with the `editTune` op's own code (`tune_edit::apply`) and render with the pitch editor's
//! renderer — the same functions `wave pitch inspect|compare|suggest|edit|render` run. The
//! rendered audio is analysed again and scored against independent truth where a dataset has
//! it. Existing Tune to Reference runs on the same cases for comparison.

use std::time::Instant;

use serde_json::{json, Value};
use wave_engine::content::mfcc;
use wave_engine::pitch::{analyze_with, F0Estimator, PitchAnalysis};
use wave_engine::pitch_api::{self, Comparison, Decision, SuggestParams};
use wave_engine::reference::tune_audio;
use wave_engine::sources::{mono_of, Audio};
use wave_engine::tune_edit::{self, NotePatch, TuneEdit};
use wave_engine::RefTune;

use crate::eval::quantile;

pub fn copy(a: &Audio) -> Audio {
    Audio::from_stereo(a.content().to_vec(), a.sr, a.channels)
}

/// How a case is corrected.
#[derive(Clone)]
pub enum Method {
    /// `pitch suggest` with these limits, its edits applied as an `editTune` op.
    Agent(SuggestParams),
    /// Existing Tune to Reference (`tune-ref` / `tune-file`).
    TuneRef(RefTune),
}

impl Method {
    pub fn label(&self) -> String {
        match self {
            Method::Agent(p) if (p.max_cents - 60.0).abs() < 1e-3 => "agent".into(),
            Method::Agent(p) => format!("agent (max {:.0} ct)", p.max_cents),
            Method::TuneRef(_) => "tune-ref".into(),
        }
    }
    pub fn is_agent(&self) -> bool {
        matches!(self, Method::Agent(_))
    }
}

pub struct Corrected {
    /// Every note's decision, as `pitch suggest` prints it (agent only).
    pub suggestions: Vec<pitch_api::Suggestion>,
    pub out: Audio,
    pub take: PitchAnalysis,
    pub cmp: Option<Comparison>,
    /// |change| cents of every edited note (agent), for the magnitude distribution.
    pub changes: Vec<f32>,
    pub kept_reasons: Vec<String>,
    pub related: Option<bool>,
    pub interval: Option<i32>,
    pub seconds: f32,
}

/// The agent workflow on two files (the CLI's steps without a project around them).
pub fn agent(take: &Audio, reference: Option<&Audio>, p: &SuggestParams, est: Option<&dyn F0Estimator>) -> Corrected {
    let t0 = Instant::now();
    let tm = mono_of(take);
    let a = analyze_with(&tm, take.sr as f32, est);
    let (notes, _) = pitch_api::clip_notes(None, &a);
    let spans: Vec<(f64, f64)> = notes.iter().map(|n| (n.start, n.end)).collect();
    let cmp = reference.map(|r| {
        let rm = mono_of(r);
        let ra = analyze_with(&rm, r.sr as f32, est);
        pitch_api::compare(&a, &mfcc(&tm, take.sr as f32), (0.0, take.seconds()), &spans, &ra, &mfcc(&rm, r.sr as f32))
    });
    let stats = pitch_api::note_stats(&a, &notes, &a.curve);
    let sug = pitch_api::suggest(&stats, &pitch_api::base_centers(&a, &notes, None), cmp.as_ref(), p);
    let edit = TuneEdit {
        init: Some(spans.iter().map(|s| [s.0, s.1]).collect()),
        notes: sug
            .iter()
            .filter(|s| s.decision == Decision::Edit)
            .map(|s| NotePatch { i: s.i, shift: s.shift, ..Default::default() })
            .collect(),
        ..Default::default()
    };
    let tune = tune_edit::apply(None, &edit).expect("the planner's edit is a valid editTune");
    let out = match &tune {
        Some(t) if !edit.notes.is_empty() => wave_engine::tune_render::render(take, &a, t, None),
        _ => copy(take),
    };
    let changes = sug.iter().filter_map(|s| s.change_cents).map(f32::abs).collect();
    let kept_reasons = sug.iter().filter(|s| s.decision == Decision::Keep).map(|s| s.reason.clone()).collect();
    let (related, interval) = (cmp.as_ref().map(|c| c.related), cmp.as_ref().and_then(|c| c.intervals.first().map(|i| i.semitones)));
    Corrected { suggestions: sug, out, take: a, cmp, changes, kept_reasons, related, interval, seconds: t0.elapsed().as_secs_f32() }
}

pub fn tune_ref(take: &Audio, reference: &Audio, p: &RefTune, est: Option<&dyn F0Estimator>) -> Corrected {
    let t0 = Instant::now();
    let (out, t, a) = tune_audio(take, reference, p, est);
    Corrected {
        suggestions: vec![],
        out,
        take: a,
        cmp: None,
        changes: vec![],
        kept_reasons: vec![],
        related: Some(t.reason.is_none()),
        interval: Some(t.octave),
        seconds: t0.elapsed().as_secs_f32(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    /// Sung as intended: must not be moved.
    InTune,
    /// A known injected error: should be corrected to `want`.
    Slip,
    /// A deliberate difference from the reference (another note, an ornament): must be kept.
    Intended,
    /// Natural singing with no known intention: reported, not scored as right or wrong.
    Natural,
}

#[derive(Clone, Debug)]
pub struct Note {
    pub start: f64,
    pub end: f64,
    pub label: Label,
    /// What a correct result sings (MIDI): the truth for in-tune and slip notes, the take for
    /// intended ones.
    pub want: f32,
}

pub struct Case<'a> {
    pub name: String,
    pub group: String,
    pub take: &'a Audio,
    pub reference: Option<&'a Audio>,
    /// Independent truth of what the take should sing, MIDI per analysis frame (NaN = unscored).
    pub truth: Vec<f32>,
    /// Frames left out of frame scoring (glides, edges).
    pub edge: Vec<bool>,
    /// What the take actually sings per frame: where its analysis is over a semitone from it, the
    /// detector slipped, which is not the correction's error.
    pub sung: Vec<f32>,
    pub notes: Vec<Note>,
    pub expect_related: Option<bool>,
    pub expect_interval: Option<i32>,
    /// Known correspondences: (take note span, reference note span), seconds.
    pub pairs: Vec<((f64, f64), (f64, f64))>,
    /// Where `truth` comes from: "annotation", "score", "original-analysis" or "none".
    pub truth_kind: &'static str,
}

#[derive(Default)]
pub struct Outcome {
    pub name: String,
    pub group: String,
    pub method: String,
    pub agent: bool,
    pub truth_kind: &'static str,
    pub failed: Option<String>,
    pub frames_before: Vec<f32>,
    pub frames_after: Vec<f32>,
    pub slip_before: Vec<f32>,
    pub slip_after: Vec<f32>,
    pub in_tune_change: Vec<f32>,
    pub intended_change: Vec<f32>,
    pub natural_change: Vec<f32>,
    pub octave_errors: usize,
    pub notes_scored: usize,
    /// (rate before, rate after, depth before, depth after) of long held notes.
    pub vibrato: Vec<(f32, f32, f32, f32)>,
    pub onset_drift_ms: Vec<f32>,
    pub onsets_lost: usize,
    pub vuv_flips: usize,
    pub vuv_frames: usize,
    pub related: Option<bool>,
    pub expect_related: Option<bool>,
    pub interval: Option<i32>,
    pub expect_interval: Option<i32>,
    pub matched: (usize, usize),
    pub changes: Vec<f32>,
    pub untouched: bool,
    pub seconds: f32,
    pub audio_seconds: f32,
    pub kept_reasons: Vec<String>,
}

fn median(mut v: Vec<f32>) -> Option<f32> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

/// Onsets (seconds) of the notes of an analysis.
fn onsets(a: &PitchAnalysis) -> Vec<f64> {
    a.notes.iter().map(|n| n.start).collect()
}

/// Writes a case's audio and the planner's decisions under `dir` (`--dump`), for listening and
/// for finding out why a case failed.
fn dump(dir: &std::path::Path, c: &Case, m: &Method, r: &Corrected) {
    let safe: String = format!("{} {} {}", c.group, c.name, m.label())
        .chars()
        .map(|ch| if ch.is_alphanumeric() || ch == '-' { ch } else { '_' })
        .collect();
    let d = dir.join(safe);
    let _ = std::fs::create_dir_all(&d);
    let w = |name: &str, a: &Audio| {
        let _ = wave_engine::sources::write_wav(&d.join(name), a.content(), a.sr);
    };
    w("take.wav", c.take);
    if let Some(r) = c.reference {
        w("reference.wav", r);
    }
    w("out.wav", &r.out);
    let notes: Vec<Value> =
        c.notes.iter().map(|n| json!({ "start": n.start, "end": n.end, "label": format!("{:?}", n.label), "want": n.want })).collect();
    let v = json!({ "case": c.name, "group": c.group, "method": m.label(), "scoredNotes": notes, "suggestions": r.suggestions, "comparison": r.cmp });
    let _ = std::fs::write(d.join("decisions.json"), serde_json::to_string_pretty(&v).unwrap_or_default());
}

pub fn run(c: &Case, m: &Method, est: Option<&dyn F0Estimator>, dump_to: Option<&std::path::Path>) -> Outcome {
    let mut o = Outcome {
        name: c.name.clone(),
        group: c.group.clone(),
        method: m.label(),
        agent: m.is_agent(),
        truth_kind: c.truth_kind,
        expect_related: c.expect_related,
        expect_interval: c.expect_interval,
        audio_seconds: c.take.seconds() as f32,
        ..Default::default()
    };
    let r = match (m, c.reference) {
        (Method::Agent(p), r) => agent(c.take, r, p, est),
        (Method::TuneRef(p), Some(r)) => tune_ref(c.take, r, p, est),
        (Method::TuneRef(_), None) => {
            o.failed = Some("tune-ref needs a reference".into());
            return o;
        }
    };
    if let Some(d) = dump_to {
        dump(d, c, m, &r);
    }
    o.seconds = r.seconds;
    o.related = r.related;
    o.interval = r.interval;
    o.changes = r.changes.clone();
    o.kept_reasons = r.kept_reasons.clone();
    o.untouched = r.out.content().iter().zip(c.take.content()).all(|(a, b)| (a - b).abs() < 1e-6);
    let ta = &r.take;
    if ta.curve.iter().all(|x| x.is_nan()) {
        o.failed = Some("nothing voiced was detected in the take".into());
        return o;
    }
    let oa = analyze_with(&mono_of(&r.out), r.out.sr as f32, est);
    let rate = ta.rate;
    let n = ta.curve.len().min(oa.curve.len());
    let slip: Vec<bool> = (0..n)
        .map(|f| matches!((ta.curve[f], c.sung.get(f)), (a, Some(&s)) if a.is_finite() && s.is_finite() && (a - s).abs() > 1.0))
        .collect();
    // frames of in-tune and slip notes, away from edges and detector slips
    let scored_note = |f: usize| {
        let t = f as f64 / rate as f64;
        c.notes.iter().any(|nt| matches!(nt.label, Label::InTune | Label::Slip) && t >= nt.start && t < nt.end)
    };
    for (f, &slipped) in slip.iter().enumerate() {
        let Some(&w) = c.truth.get(f) else { break };
        if !w.is_finite() || c.edge.get(f).copied().unwrap_or(true) || slipped || !scored_note(f) {
            continue;
        }
        if ta.curve[f].is_finite() {
            o.frames_before.push((ta.curve[f] - w).abs() * 100.0);
        }
        if oa.curve[f].is_finite() {
            o.frames_after.push((oa.curve[f] - w).abs() * 100.0);
        }
    }
    for nt in &c.notes {
        let (fa, fb) = (((nt.start + 0.05) * rate as f64) as usize, (((nt.end - 0.03) * rate as f64) as usize).min(n));
        if fb <= fa + 4 {
            continue;
        }
        let pick = |curve: &[f32]| median((fa..fb).filter(|&f| !slip[f]).map(|f| curve[f]).collect());
        let (Some(x), Some(y)) = (pick(&ta.curve), pick(&oa.curve)) else { continue };
        o.notes_scored += 1;
        if (y - nt.want).abs() > 6.0 && (y - x).abs() > 6.0 {
            o.octave_errors += 1;
        }
        let moved = (y - x).abs() * 100.0;
        match nt.label {
            Label::Slip => {
                o.slip_before.push((x - nt.want) * 100.0);
                o.slip_after.push((y - nt.want) * 100.0);
            }
            Label::InTune => o.in_tune_change.push(moved),
            Label::Intended => o.intended_change.push(moved),
            Label::Natural => o.natural_change.push(moved),
        }
        if nt.end - nt.start > 0.6 {
            let (va, vb) = (((nt.start + 0.15) * rate as f64) as usize, (((nt.end - 0.05) * rate as f64) as usize).min(n));
            if vb > va {
                if let (Some(b), Some(a)) = (pitch_api::vibrato(&ta.curve[va..vb], rate), pitch_api::vibrato(&oa.curve[va..vb], rate)) {
                    o.vibrato.push((b.0, a.0, b.1, a.1));
                }
            }
        }
    }
    // note boundaries: every onset of the take's analysis should still be there after the render
    let after = onsets(&oa);
    for t in onsets(ta) {
        match after.iter().map(|u| ((u - t) * 1000.0).abs()).min_by(f64::total_cmp) {
            Some(d) if d <= 100.0 => o.onset_drift_ms.push(d as f32),
            _ => o.onsets_lost += 1,
        }
    }
    let tol = (0.01 * rate).ceil() as usize;
    let edge = |f: usize| (f.saturating_sub(tol)..(f + tol).min(n - 1)).any(|g| ta.curve[g].is_nan() != ta.curve[g + 1].is_nan());
    o.vuv_flips = (0..n).filter(|&f| ta.curve[f].is_nan() != oa.curve[f].is_nan() && !edge(f)).count();
    o.vuv_frames = n;
    // note matching against known correspondences (agent comparisons only)
    if let Some(cmp) = &r.cmp {
        for &((ta0, ta1), (ra0, ra1)) in &c.pairs {
            let mid = (ta0 + ta1) / 2.0;
            let Some(i) = ta.notes.iter().position(|x| x.start <= mid && mid < x.end) else { continue };
            o.matched.1 += 1;
            let rmid = (ra0 + ra1) / 2.0;
            let ok = cmp.notes.get(i).and_then(|m| m.reference.first()).and_then(|&k| cmp.reference_notes.get(k)).is_some_and(|rn| {
                let ov = (rn.end.min(ra1) - rn.start.max(ra0)).max(0.0);
                (rn.start <= rmid && rmid < rn.end) || ov >= 0.5 * (rn.end - rn.start)
            });
            if ok {
                o.matched.0 += 1;
            }
        }
    }
    o
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        f32::NAN
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    }
}

fn share(v: &[f32], pred: impl Fn(f32) -> bool) -> f32 {
    if v.is_empty() {
        f32::NAN
    } else {
        v.iter().filter(|x| pred(**x)).count() as f32 / v.len() as f32
    }
}

/// Statistics of a set of outcomes (one method) with the acceptance criteria checked. A summary
/// with no case, or with any failed case, fails; criteria with no data are listed as skipped,
/// never as passed.
pub fn summarize(label: &str, os: &[&Outcome]) -> Value {
    let ok: Vec<&&Outcome> = os.iter().filter(|o| o.failed.is_none()).collect();
    let failed: Vec<Value> = os.iter().filter_map(|o| o.failed.as_ref().map(|f| json!({ "case": o.name, "why": f }))).collect();
    let cat = |f: fn(&Outcome) -> &Vec<f32>| -> Vec<f32> { ok.iter().flat_map(|o| f(o).iter().copied()).collect() };
    let (fb, fa) = (cat(|o| &o.frames_before), cat(|o| &o.frames_after));
    let (sb, sa) = (cat(|o| &o.slip_before), cat(|o| &o.slip_after));
    let (it, ic, nc) = (cat(|o| &o.in_tune_change), cat(|o| &o.intended_change), cat(|o| &o.natural_change));
    let onset = cat(|o| &o.onset_drift_ms);
    let changes = cat(|o| &o.changes);
    let abs = |v: &[f32]| v.iter().map(|x| x.abs()).collect::<Vec<_>>();
    let (sb_abs, sa_abs) = (abs(&sb), abs(&sa));
    let vib: Vec<&(f32, f32, f32, f32)> = ok.iter().flat_map(|o| o.vibrato.iter()).collect();
    let vib_rate: Vec<f32> = vib.iter().map(|v| (v.0 - v.1).abs()).collect();
    let vib_depth: Vec<f32> = vib.iter().map(|v| (v.2 - v.3).abs()).collect();
    let flips: usize = ok.iter().map(|o| o.vuv_flips).sum();
    let frames: usize = ok.iter().map(|o| o.vuv_frames).sum();
    let lost: usize = ok.iter().map(|o| o.onsets_lost).sum();
    let octave: usize = ok.iter().map(|o| o.octave_errors).sum();
    let notes_scored: usize = ok.iter().map(|o| o.notes_scored).sum();
    let iv: Vec<&&&Outcome> = ok.iter().filter(|o| o.expect_interval.is_some()).collect();
    let iv_ok = iv.iter().filter(|o| o.interval == o.expect_interval).count();
    let unrelated: Vec<&&&Outcome> = ok.iter().filter(|o| o.expect_related == Some(false)).collect();
    let held = unrelated.iter().filter(|o| o.untouched).count();
    let related_exp: Vec<&&&Outcome> = ok.iter().filter(|o| o.expect_related == Some(true)).collect();
    let related_ok = related_exp.iter().filter(|o| o.related == Some(true)).count();
    let (m_ok, m_n) = ok.iter().fold((0, 0), |(a, b), o| (a + o.matched.0, b + o.matched.1));
    let mut checks = vec![];
    let mut check = |name: &str, value: f32, limit: &str, pass: bool| {
        checks.push(json!({ "criterion": name, "value": value, "limit": limit, "pass": pass }))
    };
    check("cases ran", ok.len() as f32, ">= 1", !ok.is_empty());
    check("failed cases", failed.len() as f32, "0", failed.is_empty());
    if !fa.is_empty() && !sb.is_empty() {
        check("frame error median, stable frames (ct)", quantile(&fa, 0.5), "< 20", quantile(&fa, 0.5) < 20.0);
    }
    if !sb_abs.is_empty() {
        let red = 1.0 - mean(&sa_abs) / mean(&sb_abs);
        check("introduced error reduction (note MAE)", red, ">= 50 %", red >= 0.5);
    }
    if !it.is_empty() {
        let bad = share(&it, |x| x > 15.0);
        check("correct notes moved > 15 ct", bad, "<= 5 %", bad <= 0.05);
    }
    if !ic.is_empty() {
        let kept = share(&ic, |x| x < 20.0);
        check("intended differences kept (< 20 ct)", kept, ">= 95 %", kept >= 0.95);
    }
    if notes_scored > 0 {
        check("octave errors (notes)", octave as f32, "0", octave == 0);
    }
    if !iv.is_empty() {
        check("transposition estimated exactly", iv_ok as f32 / iv.len() as f32, "100 %", iv_ok == iv.len());
    }
    if !related_exp.is_empty() {
        check(
            "related reference recognised",
            related_ok as f32 / related_exp.len() as f32,
            ">= 90 %",
            related_ok * 10 >= related_exp.len() * 9,
        );
    }
    if !unrelated.is_empty() {
        check("unrelated reference → audio untouched", held as f32 / unrelated.len() as f32, "100 %", held == unrelated.len());
    }
    if m_n > 0 {
        check("note matching accuracy", m_ok as f32 / m_n as f32, ">= 80 %", m_ok * 5 >= m_n * 4);
    }
    if !vib_rate.is_empty() {
        check("vibrato rate change P90 (Hz)", quantile(&vib_rate, 0.9), "< 0.5", quantile(&vib_rate, 0.9) < 0.5);
        check("vibrato depth change P90 (ct)", quantile(&vib_depth, 0.9), "< 15", quantile(&vib_depth, 0.9) < 15.0);
    }
    if !onset.is_empty() {
        check("note onset drift P90 (ms)", quantile(&onset, 0.9), "< 25", quantile(&onset, 0.9) < 25.0);
    }
    if frames > 0 {
        check("V/UV corruption (off edges ±10 ms)", flips as f32 / frames as f32, "< 1 %", (flips as f32) < frames as f32 * 0.01);
    }
    let buckets = [5.0, 15.0, 30.0, 50.0, 100.0, 200.0, f32::INFINITY];
    let mut hist = vec![0usize; buckets.len()];
    for c in &changes {
        hist[buckets.iter().position(|b| c < b).unwrap_or(buckets.len() - 1)] += 1;
    }
    let names = ["<5", "5-15", "15-30", "30-50", "50-100", "100-200", ">=200"];
    let histogram: Vec<Value> = names.iter().zip(&hist).map(|(k, v)| json!({ "range": k, "notes": v })).collect();
    let audio: f32 = ok.iter().map(|o| o.audio_seconds).sum();
    let secs: f32 = ok.iter().map(|o| o.seconds).sum();
    json!({
        "label": label,
        "method": os.first().map(|o| o.method.clone()),
        "informational": os.first().is_some_and(|o| !o.agent),
        "truth": os.first().map(|o| o.truth_kind),
        "cases": os.len(),
        "failed": failed,
        "frames": { "count": fa.len(), "medianBefore": quantile(&fb, 0.5), "median": quantile(&fa, 0.5), "maeBefore": mean(&fb), "mae": mean(&fa), "p90Before": quantile(&fb, 0.9), "p90": quantile(&fa, 0.9) },
        "slips": { "count": sb.len(), "maeBefore": mean(&sb_abs), "mae": mean(&sa_abs), "medianAfter": quantile(&sa_abs, 0.5), "p90After": quantile(&sa_abs, 0.9), "fixedWithin20ct": share(&sa_abs, |x| x < 20.0) },
        "inTune": { "count": it.len(), "movedOver15ct": share(&it, |x| x > 15.0), "medianMove": quantile(&it, 0.5), "p90Move": quantile(&it, 0.9) },
        "intended": { "count": ic.len(), "keptWithin20ct": share(&ic, |x| x < 20.0), "p90Move": quantile(&ic, 0.9) },
        "natural": { "count": nc.len(), "movedOver15ct": share(&nc, |x| x > 15.0), "medianMove": quantile(&nc, 0.5), "p90Move": quantile(&nc, 0.9) },
        "octaveErrors": octave,
        "notesScored": notes_scored,
        "interval": { "cases": iv.len(), "exact": iv_ok },
        "relatedRecognised": format!("{related_ok}/{}", related_exp.len()),
        "unrelatedUntouched": format!("{held}/{}", unrelated.len()),
        "noteMatching": { "correct": m_ok, "of": m_n },
        "vibrato": { "notes": vib_rate.len(), "rateChangeP90": quantile(&vib_rate, 0.9), "depthChangeP90": quantile(&vib_depth, 0.9), "depthChangeMedian": quantile(&vib_depth, 0.5) },
        "onsets": { "count": onset.len(), "lost": lost, "driftMedianMs": quantile(&onset, 0.5), "driftP90Ms": quantile(&onset, 0.9) },
        "vuv": { "flips": flips, "frames": frames },
        "corrections": { "count": changes.len(), "histogramCents": histogram },
        "runtime": { "secondsTotal": secs, "audioSeconds": audio, "realtimeFactor": if audio > 0.0 { secs / audio } else { f32::NAN } },
        "checks": checks,
    })
}

pub fn outcome_json(o: &Outcome) -> Value {
    json!({
        "name": o.name, "group": o.group, "method": o.method, "failed": o.failed,
        "related": o.related, "expectRelated": o.expect_related, "interval": o.interval, "expectInterval": o.expect_interval,
        "untouched": o.untouched,
        "frameMedianBefore": quantile(&o.frames_before, 0.5), "frameMedian": quantile(&o.frames_after, 0.5),
        "slipMaeBefore": mean(&o.slip_before.iter().map(|x| x.abs()).collect::<Vec<_>>()), "slipMae": mean(&o.slip_after.iter().map(|x| x.abs()).collect::<Vec<_>>()),
        "inTuneMoved": o.in_tune_change.iter().filter(|x| **x > 15.0).count(), "inTune": o.in_tune_change.len(),
        "intendedMoved": o.intended_change.iter().filter(|x| **x >= 20.0).count(), "intended": o.intended_change.len(),
        "matched": [o.matched.0, o.matched.1], "edits": o.changes.len(), "seconds": o.seconds,
        "keptBecause": summarize_reasons(&o.kept_reasons),
    })
}

/// The planner's reasons for keeping notes, grouped by their first words.
fn summarize_reasons(r: &[String]) -> Value {
    let mut m: std::collections::BTreeMap<String, usize> = Default::default();
    for x in r {
        let k: String =
            x.split(|c: char| c == '(' || c == ':' || c.is_ascii_digit() || c == '+' || c == '-').next().unwrap_or("").trim().to_string();
        *m.entry(k).or_default() += 1;
    }
    json!(m)
}

/// Every method's summary per group and overall.
pub fn summaries(suite: &str, os: &[Outcome]) -> Vec<Value> {
    let mut methods: Vec<String> = os.iter().map(|o| o.method.clone()).collect();
    methods.sort();
    methods.dedup();
    let mut groups: Vec<String> = os.iter().map(|o| o.group.clone()).collect();
    groups.sort();
    groups.dedup();
    let mut out = vec![];
    for m in &methods {
        for g in &groups {
            let set: Vec<&Outcome> = os.iter().filter(|o| &o.method == m && &o.group == g).collect();
            if !set.is_empty() {
                out.push(summarize(&format!("{suite} · {g} · {m}"), &set));
            }
        }
    }
    out
}
