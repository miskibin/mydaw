//! Blind A/B listening kit: per fragment, Tune to Reference against a baseline
//! (classic note-snap autotune, or the uncorrected take), loudness-matched, in random
//! order, with the answer key kept apart. `score` turns listeners' choices into a
//! preference rate with an exact two-sided binomial test.

use std::path::Path;

use serde_json::{json, Value};
use wave_engine::pitch::{analyze_with, F0Estimator};
use wave_engine::reference::tune_audio;
use wave_engine::singer::{sing, song, Voice};
use wave_engine::sources::{mono_of, write_wav, Audio};
use wave_engine::{ClipTune, NoteEdit, RefTune};

use crate::eval::mono_audio;
use crate::vocalset::{self, Item};
use crate::Rng;

const MAX_SECONDS: f64 = 8.0;

/// Classic pitch correction: every detected note moved to the nearest semitone,
/// its vibrato kept: what a singer gets without a reference.
fn snapped(take: &Audio, est: Option<&dyn F0Estimator>) -> Audio {
    let a = analyze_with(&mono_of(take), take.sr as f32, est);
    let notes = a
        .notes
        .iter()
        .map(|n| NoteEdit {
            start: n.start,
            end: n.end,
            shift: (n.pitch.round() - n.pitch) * 100.0,
            drift: 0.0,
            formant: 0.0,
            gain: 0.0,
            speed: None,
        })
        .collect();
    wave_engine::tune_render::render(take, &a, &ClipTune { notes, pencil: vec![], reference: None }, None)
}

fn trimmed(a: &Audio) -> Audio {
    let n = (a.frames as f64).min(MAX_SECONDS * a.sr as f64) as usize;
    Audio::from_stereo(a.content()[..n * 2].to_vec(), a.sr, a.channels)
}

/// RMS over the active 20 ms blocks (gate −50 dBFS).
fn active_rms(x: &[f32], sr: u32) -> f32 {
    let block = (sr as usize / 50).max(1) * 2;
    let ms: Vec<f32> = x.chunks(block).map(|c| c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).filter(|m| *m > 1e-5).collect();
    if ms.is_empty() {
        return 0.0;
    }
    (ms.iter().sum::<f32>() / ms.len() as f32).sqrt()
}

/// Gains that bring both to −20 dBFS active RMS, lowered together if either would clip.
fn matched(a: &Audio, b: &Audio) -> (Vec<f32>, Vec<f32>) {
    let target = 0.1f32;
    let ga = target / active_rms(a.content(), a.sr).max(1e-6);
    let gb = target / active_rms(b.content(), b.sr).max(1e-6);
    let peak = (a.peak() * ga).max(b.peak() * gb);
    let k = if peak > 0.98 { 0.98 / peak } else { 1.0 };
    (a.content().iter().map(|x| x * ga * k).collect(), b.content().iter().map(|x| x * gb * k).collect())
}

/// (name, take, reference) fragments: from VocalSet when given, else synthetic.
fn fragments(root: Option<&Path>, n: usize, est: Option<&dyn F0Estimator>) -> Result<Vec<(String, Audio, Audio)>, String> {
    let mut out = vec![];
    if let Some(root) = root {
        let items = vocalset::index(root)?;
        let pairs = vocalset::cross_pairs(&items);
        let picked: Vec<&(&Item, &Item)> = vocalset::spread(&pairs, n);
        for (i, (ti, ri)) in picked.into_iter().enumerate() {
            let (Ok(take), Ok(reference)) = (Audio::load(&ti.audio), Audio::load(&ri.audio)) else {
                continue;
            };
            // a natural take, nudged off by 30–90 cents per note so there is something to fix
            let level = [30.0, 50.0, 70.0, 90.0][i % 4];
            let a = analyze_with(&mono_of(&take), take.sr as f32, est);
            let notes = a
                .notes
                .iter()
                .enumerate()
                .map(|(k, nb)| NoteEdit {
                    start: nb.start,
                    end: nb.end,
                    shift: if k % 2 == 0 { level } else { -level },
                    drift: 0.0,
                    formant: 0.0,
                    gain: 0.0,
                    speed: None,
                })
                .collect();
            let off = wave_engine::tune_render::render(&take, &a, &ClipTune { notes, pencil: vec![], reference: None }, None);
            out.push((format!("{} ← {}", ti.stem, ri.stem), trimmed(&off), trimmed(&reference)));
        }
    } else {
        let sr = 44100.0;
        for s in 0..n as u32 {
            let melody = song(500 + s, 12);
            let mut rng = Rng(31 * (s + 1));
            let detune: Vec<f32> = (0..melody.len()).map(|_| rng.sym() * 80.0).collect();
            let take = sing(&melody, &Voice { seed: s + 3, ..Voice::default() }, &detune, 10.0, sr);
            let reference = sing(
                &melody,
                &Voice { formant: 1.15, tempo: 0.92, jitter: 0.1, transpose: 0.0, vib_rate: 6.0, vib_depth: 0.25, seed: 50 + s },
                &[],
                0.0,
                sr,
            );
            out.push((
                format!("synthetic song {s}"),
                trimmed(&mono_audio(&take.audio, sr as u32)),
                trimmed(&mono_audio(&reference.audio, sr as u32)),
            ));
        }
    }
    Ok(out)
}

pub fn make(
    out_dir: &Path,
    root: Option<&Path>,
    n: usize,
    baseline: &str,
    seed: u32,
    p: &RefTune,
    est: Option<&dyn F0Estimator>,
) -> Result<Value, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let frags = fragments(root, n, est)?;
    let mut rng = Rng(seed.max(1));
    let mut key = String::from("fragment,source,A,B,score\n");
    let mut template = String::from("listener,fragment,choice\n");
    for (i, (name, take, reference)) in frags.iter().enumerate() {
        let (ours, t, _) = tune_audio(take, reference, p, est);
        let base = if baseline == "take" { trimmed(take) } else { snapped(take, est) };
        let (x, y) = matched(&ours, &base);
        let ours_first = rng.unit() < 0.5;
        let (a, b) = if ours_first { (&x, &y) } else { (&y, &x) };
        let id = format!("{:02}", i + 1);
        write_wav(&out_dir.join(format!("{id}_A.wav")), a, take.sr)?;
        write_wav(&out_dir.join(format!("{id}_B.wav")), b, take.sr)?;
        let (r, _) = matched(reference, reference);
        write_wav(&out_dir.join(format!("{id}_reference.wav")), &r, reference.sr)?;
        let (na, nb) = if ours_first { ("tune-to-reference", baseline) } else { (baseline, "tune-to-reference") };
        key += &format!("{id},\"{name}\",{na},{nb},{:.2}\n", t.score);
        template += &format!("listener1,{id},\n");
        eprintln!("  {id} {name} (score {:.2})", t.score);
    }
    std::fs::write(out_dir.join("key.csv"), key).map_err(|e| e.to_string())?;
    std::fs::write(out_dir.join("ratings.csv"), template).map_err(|e| e.to_string())?;
    std::fs::write(
        out_dir.join("README.txt"),
        format!(
            "Blind A/B test — Tune to Reference vs {baseline}\n\n\
             For each fragment NN: listen to NN_reference.wav (the melody as it should go), then NN_A.wav and NN_B.wav\n\
             (same take, two corrections, matched in loudness, order random). Write A or B in ratings.csv for the one that\n\
             sounds better (more in tune with the reference, and natural). One row per listener and fragment; copy the rows\n\
             for each listener and change the listener name. Do not open key.csv until everyone is done.\n\n\
             Then: tune-bench ab-score --kit <this folder> --ratings ratings.csv\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({ "kit": out_dir.display().to_string(), "fragments": frags.len(), "baseline": baseline }))
}

/// P(X ≥ k or X ≤ n−k) for X ~ Binomial(n, ½), k ≥ n/2: the two-sided exact p-value.
fn binomial_two_sided(k: usize, n: usize) -> f64 {
    let k = k.max(n - k);
    let mut ln_c = vec![0f64; n + 1];
    for i in 1..=n {
        ln_c[i] = ln_c[i - 1] + ((n - i + 1) as f64).ln() - (i as f64).ln();
    }
    let tail: f64 = (k..=n).map(|i| (ln_c[i] - n as f64 * 2f64.ln()).exp()).sum();
    (2.0 * tail).min(1.0)
}

pub fn score(kit: &Path, ratings: &Path) -> Result<Value, String> {
    let key = std::fs::read_to_string(kit.join("key.csv")).map_err(|e| format!("key.csv: {e}"))?;
    let ours_is: std::collections::HashMap<String, char> = key
        .lines()
        .skip(1)
        .filter_map(|l| {
            let id = l.split(',').next()?.to_string();
            let a_is_ours = l.split(',').nth_back(2) == Some("tune-to-reference");
            Some((id, if a_is_ours { 'A' } else { 'B' }))
        })
        .collect();
    let text = std::fs::read_to_string(ratings).map_err(|e| format!("{}: {e}", ratings.display()))?;
    let (mut wins, mut total) = (0usize, 0usize);
    let mut per: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for l in text.lines().skip(1) {
        let c: Vec<&str> = l.split(',').map(str::trim).collect();
        let (Some(who), Some(id), Some(choice)) = (c.first(), c.get(1), c.get(2).and_then(|s| s.chars().next())) else {
            continue;
        };
        let Some(&ours) = ours_is.get(*id) else {
            continue;
        };
        let choice = choice.to_ascii_uppercase();
        if choice != 'A' && choice != 'B' {
            continue;
        }
        let e = per.entry(who.to_string()).or_default();
        e.1 += 1;
        total += 1;
        if choice == ours {
            e.0 += 1;
            wins += 1;
        }
    }
    if total == 0 {
        return Err("no ratings (fill the choice column with A or B)".into());
    }
    Ok(json!({
        "ratings": total,
        "listeners": per.len(),
        "tuneToReferencePreferred": wins as f64 / total as f64,
        "pValue": binomial_two_sided(wins, total),
        "perListener": per.iter().map(|(k, (w, n))| json!({ "listener": k, "preferred": *w as f64 / *n as f64, "ratings": n })).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn binomial_p_values() {
        assert!((super::binomial_two_sided(10, 20) - 1.0).abs() < 1e-9);
        // 15 of 20: p ≈ 0.0414
        assert!((super::binomial_two_sided(15, 20) - 0.04139).abs() < 1e-4);
        assert!((super::binomial_two_sided(5, 20) - 0.04139).abs() < 1e-4);
    }
}
