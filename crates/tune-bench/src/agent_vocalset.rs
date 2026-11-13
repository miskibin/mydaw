//! `agent-vocalset`: the agent workflow on Annotated VocalSet (real singers, hand-checked F0 and
//! notes). Controlled groups (a real take with known errors against its untouched self), real
//! cross-singer pairs (another singer, range and timing as the reference), the same take
//! against a reference in another key, and unrelated references.

use wave_engine::pitch::F0Estimator;
use wave_engine::sources::Audio;

use crate::agent::Label;
use crate::cases::{inject, plan, transposed, Owned, SELF_GROUPS};
use crate::vocalset::{cross_pairs, edges, frames_of, note_pitch, spread, Item};

fn load(it: &Item) -> Result<Audio, String> {
    Audio::load(&it.audio).map_err(|e| format!("{}: {e}", it.audio.display()))
}

/// Builds the cases; a recording that cannot be loaded becomes a failed case, not a gap.
pub fn cases(items: &[Item], limit: usize, est: Option<&dyn F0Estimator>, failed: &mut Vec<(String, String)>) -> Vec<Owned> {
    let mut out = vec![];
    let exercises: Vec<&Item> = items.iter().filter(|i| !i.is_excerpt()).collect();
    // 1. controlled: the take against its own untouched recording
    for (i, it) in spread(&exercises, limit).into_iter().enumerate() {
        let audio = match load(it) {
            Ok(a) => a,
            Err(e) => {
                failed.push((it.stem.clone(), e));
                continue;
            }
        };
        let group = SELF_GROUPS[i % SELF_GROUPS.len()];
        let frames = frames_of(&audio);
        let truth = it.curve(frames);
        let p = plan(group, it.notes.len(), i);
        let nt: Vec<Option<f32>> = it.notes.iter().map(|&n| note_pitch(&truth, n)).collect();
        let drift = if group.contains("drift") { 30.0 } else { 0.0 };
        let (take, sung, notes) = inject(&audio, &it.notes, &nt, &truth, &p, drift, est);
        out.push(Owned {
            name: it.stem.clone(),
            group: format!("self: {group}"),
            take,
            reference: Some(audio),
            edge: edges(&truth, &it.notes),
            truth,
            sung,
            notes,
            expect_related: Some(true),
            expect_interval: Some(0),
            pairs: it.notes.iter().map(|&n| (n, n)).collect(),
            truth_kind: "annotation",
        });
    }
    // 2. real cross-singer pairs and 3. the same take against itself in another key
    let pairs = cross_pairs(items);
    for (i, (ti, ri)) in spread(&pairs, limit).into_iter().enumerate() {
        let (orig, reference) = match (load(ti), load(ri)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                failed.push((format!("{} ← {}", ti.stem, ri.stem), e));
                continue;
            }
        };
        let frames = frames_of(&orig);
        let truth = ti.curve(frames);
        let truth_ref = ri.curve(frames_of(&reference));
        let p = plan("slips", ti.notes.len(), i);
        let nt: Vec<Option<f32>> = ti.notes.iter().map(|&n| note_pitch(&truth, n)).collect();
        let (take, sung, notes) = inject(&orig, &ti.notes, &nt, &truth, &p, 0.0, est);
        let diffs: Vec<f32> =
            ti.notes.iter().zip(&ri.notes).filter_map(|(&a, &b)| Some(note_pitch(&truth, a)? - note_pitch(&truth_ref, b)?)).collect();
        let expect = crate::cases::median(diffs).map(|d| d.round() as i32);
        out.push(Owned {
            name: format!("{} ← {}", ti.stem, ri.stem),
            group: "cross-singer".into(),
            take,
            reference: Some(reference),
            edge: edges(&truth, &ti.notes),
            truth: truth.clone(),
            sung: sung.clone(),
            notes: notes.clone(),
            expect_related: Some(true),
            expect_interval: expect,
            pairs: ti.notes.iter().copied().zip(ri.notes.iter().copied()).collect(),
            truth_kind: "annotation",
        });
        if i % 2 == 0 {
            let t = [3.0f32, -5.0, 7.0, -12.0, 4.0, -2.0][(i / 2) % 6];
            let (take2, sung2, notes2) = inject(&orig, &ti.notes, &nt, &truth, &p, 0.0, est);
            out.push(Owned {
                name: format!("{} ← itself {t:+} st", ti.stem),
                group: "reference in another key".into(),
                take: take2,
                reference: Some(transposed(&orig, t, est)),
                edge: edges(&truth, &ti.notes),
                truth,
                sung: sung2,
                notes: notes2,
                expect_related: Some(true),
                expect_interval: Some(-t as i32),
                pairs: ti.notes.iter().map(|&n| (n, n)).collect(),
                truth_kind: "annotation",
            });
        }
    }
    // 4. unrelated references: an exercise against a song excerpt
    let excerpts: Vec<&Item> = items.iter().filter(|i| i.is_excerpt()).collect();
    if !excerpts.is_empty() {
        for (i, ti) in spread(&exercises, (limit / 2).max(4)).into_iter().enumerate() {
            let ri = excerpts[(i * 7) % excerpts.len()];
            let (orig, reference) = match (load(ti), load(ri)) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) => {
                    failed.push((format!("{} ← {}", ti.stem, ri.stem), e));
                    continue;
                }
            };
            let frames = frames_of(&orig);
            let truth = ti.curve(frames);
            let p = plan("slips", ti.notes.len(), i);
            let nt: Vec<Option<f32>> = ti.notes.iter().map(|&n| note_pitch(&truth, n)).collect();
            let (take, sung, notes) = inject(&orig, &ti.notes, &nt, &truth, &p, 0.0, est);
            // an unrelated reference must change nothing, so every note is scored as "keep"
            let notes = notes.into_iter().map(|mut n| {
                n.label = Label::Natural;
                n
            });
            out.push(Owned {
                name: format!("{} ← {}", ti.stem, ri.stem),
                group: "unrelated reference".into(),
                take,
                reference: Some(reference),
                edge: edges(&truth, &ti.notes),
                truth,
                sung,
                notes: notes.collect(),
                expect_related: Some(false),
                expect_interval: None,
                pairs: vec![],
                truth_kind: "annotation",
            });
        }
    }
    out
}

/// The detector against the hand-checked F0 of `limit` recordings: raw pitch accuracy within
/// 50 cents over annotated voiced frames, cents error, voicing recall and false alarms.
pub fn estimation(items: &[Item], limit: usize, est: Option<&dyn F0Estimator>) -> serde_json::Value {
    use wave_engine::pitch::analyze_with;
    use wave_engine::sources::mono_of;
    let (mut voiced, mut hit, mut unv, mut fa, mut errs, mut missing) = (0usize, 0usize, 0usize, 0usize, vec![], vec![]);
    for it in spread(items, limit) {
        let Ok(a) = load(it) else {
            missing.push(it.stem.clone());
            continue;
        };
        let an = analyze_with(&mono_of(&a), a.sr as f32, est);
        let truth = it.curve(an.curve.len());
        for (t, d) in truth.iter().zip(&an.curve) {
            match (t.is_finite(), d.is_finite()) {
                (true, true) => {
                    voiced += 1;
                    hit += 1;
                    errs.push(((d - t) * 100.0).abs());
                }
                (true, false) => voiced += 1,
                (false, true) => {
                    unv += 1;
                    fa += 1;
                }
                (false, false) => unv += 1,
            }
        }
    }
    serde_json::json!({
        "recordings": spread(items, limit).len(), "missing": missing, "annotatedVoicedFrames": voiced,
        "rawPitchAccuracy50ct": errs.iter().filter(|e| **e < 50.0).count() as f32 / voiced.max(1) as f32,
        "medianAbsCents": crate::eval::quantile(&errs, 0.5), "p90AbsCents": crate::eval::quantile(&errs, 0.9),
        "octaveErrors": errs.iter().filter(|e| **e > 600.0).count(),
        "voicingRecall": hit as f32 / voiced.max(1) as f32, "voicingFalseAlarm": fa as f32 / unv.max(1) as f32,
    })
}
