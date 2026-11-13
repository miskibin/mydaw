//! Blind listening kit, three conditions per fragment: the take as recorded (`original`),
//! existing Tune to Reference (`tune-ref`) and the agent workflow (`agent`), loudness-matched and
//! in random order, with the reference to compare against. Real recordings only: VocalSet
//! cross-singer pairs (with mild known slips, so there is something to fix) and GTSinger
//! tenor/alto phrases as sung. `score` aggregates listeners' 1–5 ratings; it never invents any.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};
use wave_engine::pitch::F0Estimator;
use wave_engine::pitch_api::SuggestParams;
use wave_engine::sources::{write_wav, Audio};
use wave_engine::RefTune;

use crate::ab::{active_rms, binomial_two_sided, trimmed};
use crate::agent::{agent, copy, tune_ref};
use crate::cases::{inject, plan};
use crate::Rng;

pub const CONDITIONS: [&str; 3] = ["original", "tune-ref", "agent"];

/// Gains bringing every version to −20 dBFS active RMS, lowered together if any would clip.
fn matched(xs: &[&Audio]) -> Vec<Vec<f32>> {
    let g: Vec<f32> = xs.iter().map(|a| 0.1 / active_rms(a.content(), a.sr).max(1e-6)).collect();
    let peak = xs.iter().zip(&g).map(|(a, g)| a.peak() * g).fold(0f32, f32::max);
    let k = if peak > 0.98 { 0.98 / peak } else { 1.0 };
    xs.iter().zip(&g).map(|(a, g)| a.content().iter().map(|x| x * g * k).collect()).collect()
}

fn fragments(
    vocalset: Option<&Path>,
    gtsinger: Option<&Path>,
    n: usize,
    est: Option<&dyn F0Estimator>,
) -> Result<Vec<(String, Audio, Audio)>, String> {
    let mut out = vec![];
    if let Some(root) = vocalset {
        let items = crate::vocalset::index(root)?;
        let pairs = crate::vocalset::cross_pairs(&items);
        for (i, (ti, ri)) in crate::vocalset::spread(&pairs, n.div_ceil(2)).into_iter().enumerate() {
            let (Ok(a), Ok(r)) = (Audio::load(&ti.audio), Audio::load(&ri.audio)) else { continue };
            let frames = crate::vocalset::frames_of(&a);
            let truth = ti.curve(frames);
            let nt: Vec<Option<f32>> = ti.notes.iter().map(|&s| crate::vocalset::note_pitch(&truth, s)).collect();
            let (take, _, _) = inject(&a, &ti.notes, &nt, &truth, &plan("slips", ti.notes.len(), i), 0.0, est);
            out.push((format!("VocalSet {} ← {} (slips on every third note)", ti.stem, ri.stem), trimmed(&take), trimmed(&r)));
        }
    }
    if let Some(root) = gtsinger {
        let segs = crate::gtsinger::index(root)?;
        let plain: Vec<&crate::gtsinger::Seg> = segs.iter().filter(|s| s.group == "Control_Group").collect();
        let mut pairs = vec![];
        for a in &plain {
            if let Some(b) = plain.iter().find(|b| b.singer != a.singer && b.song == a.song && b.tech == a.tech && b.id == a.id) {
                pairs.push((*a, *b));
            }
        }
        for (a, b) in crate::vocalset::spread(&pairs, n / 2) {
            let (Ok(x), Ok(y)) = (Audio::load(&a.wav), Audio::load(&b.wav)) else { continue };
            out.push((format!("GTSinger {} ← {} (as sung)", a.name(), b.singer), trimmed(&x), trimmed(&y)));
        }
    }
    if out.is_empty() {
        return Err("no fragments: pass --vocalset and/or --gtsinger".into());
    }
    Ok(out)
}

pub fn make(
    dir: &Path,
    vocalset: Option<&Path>,
    gtsinger: Option<&Path>,
    n: usize,
    seed: u32,
    p: &RefTune,
    est: Option<&dyn F0Estimator>,
) -> Result<Value, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let frags = fragments(vocalset, gtsinger, n, est)?;
    let mut rng = Rng(seed.max(1));
    let mut key = String::from("fragment,source,A,B,C\n");
    let mut template = String::from("listener,fragment,A,B,C,best\n");
    for (i, (name, take, reference)) in frags.iter().enumerate() {
        let tr = tune_ref(take, reference, p, est).out;
        let ag = agent(take, Some(reference), &SuggestParams::default(), est).out;
        let orig = copy(take);
        let versions = [&orig, &tr, &ag];
        let lv = matched(&versions);
        // a random order of the three (Fisher–Yates)
        let mut order = [0usize, 1, 2];
        for k in (1..3).rev() {
            let j = ((rng.unit() * (k + 1) as f32) as usize).min(k);
            order.swap(k, j);
        }
        let id = format!("{:02}", i + 1);
        for (slot, &v) in ["A", "B", "C"].iter().zip(&order) {
            write_wav(&dir.join(format!("{id}_{slot}.wav")), &lv[v], take.sr)?;
        }
        let r = matched(&[reference]).remove(0);
        write_wav(&dir.join(format!("{id}_reference.wav")), &r, reference.sr)?;
        key += &format!("{id},\"{}\",{},{},{}\n", name.replace('"', "'"), CONDITIONS[order[0]], CONDITIONS[order[1]], CONDITIONS[order[2]]);
        template += &format!("listener1,{id},,,,\n");
        eprintln!("  {id} {name}");
    }
    std::fs::write(dir.join("key.csv"), key).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("ratings.csv"), template).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("README.txt"),
        "Blind listening test — three versions of each vocal fragment\n\n\
         For each fragment NN: listen to NN_reference.wav (another singer performing the same melody), then NN_A.wav,\n\
         NN_B.wav and NN_C.wav: the same take, processed three ways, matched in loudness, in random order.\n\
         In ratings.csv give each of A, B and C a score from 1 (bad) to 5 (excellent) for how good the vocal sounds:\n\
         in tune where it should be, and still natural (its own vibrato, slides and character). Optionally write the\n\
         letter of your favourite in `best`. One row per listener and fragment: copy the rows for each listener and\n\
         change the listener name. Do not open key.csv until everyone is done.\n\n\
         Then: tune-bench listen-score --kit <this folder> --ratings ratings.csv\n",
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({ "kit": dir.display().to_string(), "fragments": frags.len(), "conditions": CONDITIONS }))
}

pub fn score(kit: &Path, ratings: &Path) -> Result<Value, String> {
    let key = std::fs::read_to_string(kit.join("key.csv")).map_err(|e| format!("key.csv: {e}"))?;
    let mut slots: BTreeMap<String, [String; 3]> = BTreeMap::new();
    for l in key.lines().skip(1) {
        let c: Vec<&str> = l.split(',').collect();
        if c.len() < 5 {
            continue;
        }
        let n = c.len();
        slots.insert(c[0].to_string(), [c[n - 3].trim().to_string(), c[n - 2].trim().to_string(), c[n - 1].trim().to_string()]);
    }
    let text = std::fs::read_to_string(ratings).map_err(|e| format!("{}: {e}", ratings.display()))?;
    let mut scores: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut best: BTreeMap<String, usize> = BTreeMap::new();
    let mut per_listener: BTreeMap<String, BTreeMap<String, Vec<f64>>> = BTreeMap::new();
    // pairwise: (a, b) → (a higher, b higher, ties)
    let mut pairs: BTreeMap<(String, String), (usize, usize, usize)> = BTreeMap::new();
    let (mut rows, mut skipped) = (0usize, 0usize);
    for l in text.lines().skip(1) {
        let c: Vec<&str> = l.split(',').map(str::trim).collect();
        let (Some(who), Some(id)) = (c.first(), c.get(1)) else { continue };
        let Some(cond) = slots.get(*id) else {
            skipped += 1;
            continue;
        };
        let vals: Vec<Option<f64>> =
            (2..5).map(|k| c.get(k).and_then(|s| s.parse::<f64>().ok()).filter(|v| (1.0..=5.0).contains(v))).collect();
        if vals.iter().any(Option::is_none) {
            skipped += 1;
            continue;
        }
        rows += 1;
        let v: Vec<f64> = vals.into_iter().flatten().collect();
        for (k, s) in v.iter().enumerate() {
            scores.entry(cond[k].clone()).or_default().push(*s);
            per_listener.entry(who.to_string()).or_default().entry(cond[k].clone()).or_default().push(*s);
        }
        if let Some(b) = c.get(5).and_then(|s| s.chars().next()).map(|ch| ch.to_ascii_uppercase()) {
            if let Some(k) = "ABC".find(b) {
                *best.entry(cond[k].clone()).or_default() += 1;
            }
        }
        let idx = |name: &str| cond.iter().position(|x| x == name);
        for (x, y) in [("agent", "tune-ref"), ("agent", "original"), ("tune-ref", "original")] {
            if let (Some(i), Some(j)) = (idx(x), idx(y)) {
                let e = pairs.entry((x.to_string(), y.to_string())).or_default();
                match v[i].total_cmp(&v[j]) {
                    std::cmp::Ordering::Greater => e.0 += 1,
                    std::cmp::Ordering::Less => e.1 += 1,
                    std::cmp::Ordering::Equal => e.2 += 1,
                }
            }
        }
    }
    if rows == 0 {
        return Err("no complete ratings (each row needs A, B and C scored 1–5)".into());
    }
    let mean = |v: &Vec<f64>| v.iter().sum::<f64>() / v.len().max(1) as f64;
    Ok(json!({
        "ratings": rows,
        "skippedRows": skipped,
        "listeners": per_listener.len(),
        "meanScore": scores.iter().map(|(k, v)| (k.clone(), json!({ "mean": mean(v), "n": v.len() }))).collect::<serde_json::Map<_, _>>(),
        "chosenBest": best,
        "pairwise": pairs.iter().map(|((x, y), (a, b, t))| json!({
            "a": x, "b": y, "aHigher": a, "bHigher": b, "ties": t,
            "signTestP": if a + b > 0 { binomial_two_sided(*a.max(b), a + b) } else { 1.0 },
        })).collect::<Vec<_>>(),
        "perListener": per_listener.iter().map(|(w, m)| json!({ "listener": w, "mean": m.iter().map(|(k, v)| (k.clone(), json!(mean(v)))).collect::<serde_json::Map<_, _>>() })).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn scores_are_aggregated_per_condition() {
        let d = std::env::temp_dir().join(format!("wave-listen-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("key.csv"), "fragment,source,A,B,C\n01,\"x\",agent,original,tune-ref\n02,\"y\",original,tune-ref,agent\n")
            .unwrap();
        std::fs::write(d.join("r.csv"), "listener,fragment,A,B,C,best\nann,01,5,3,4,A\nann,02,2,4,5,C\nbob,01,4,,4,\n").unwrap();
        let v = super::score(&d, &d.join("r.csv")).unwrap();
        assert_eq!(v["ratings"], 2);
        assert_eq!(v["skippedRows"], 1, "an incomplete row is skipped and counted, not guessed");
        assert_eq!(v["meanScore"]["agent"]["mean"], 5.0);
        assert_eq!(v["meanScore"]["original"]["mean"], 2.5);
        assert_eq!(v["chosenBest"]["agent"], 2);
        std::fs::remove_dir_all(&d).ok();
    }
}
