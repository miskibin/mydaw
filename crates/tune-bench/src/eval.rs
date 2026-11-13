//! One benchmark case run end to end (analysis → alignment → target → render →
//! re-analysis of the render) and the statistics the acceptance criteria use.

use std::time::Instant;

use serde_json::{json, Value};
use wave_engine::pitch::{analyze_with, F0Estimator, PitchAnalysis};
use wave_engine::reference::tune_audio;
use wave_engine::sources::{mono_of, Audio};
use wave_engine::RefTune;

pub struct Case<'a> {
    pub name: String,
    pub group: String,
    pub take: &'a Audio,
    pub reference: &'a Audio,
    /// What the corrected take should sing, MIDI per analysis frame; NaN = not scored.
    pub truth: Vec<f32>,
    /// Frames left out of frame scoring (glides, note edges).
    pub edge: Vec<bool>,
    /// What the take actually sings; where its analysis is over a semitone away the
    /// detector slipped, and those frames are the detector's error, not the correction's.
    pub sung: Vec<f32>,
    /// Scored notes: (start, end, wanted MIDI) in seconds of the take.
    pub notes: Vec<(f64, f64, f32)>,
    /// (take onset, matching reference onset) in seconds.
    pub onsets: Vec<(f64, f64)>,
    pub expect_rejection: bool,
}

pub struct Outcome {
    pub name: String,
    pub group: String,
    pub expect_rejection: bool,
    pub corrected: bool,
    pub untouched: bool,
    pub score: f32,
    pub octave: i32,
    pub reason: Option<String>,
    pub frames_before: Vec<f32>,
    pub frames_after: Vec<f32>,
    pub notes_before: Vec<f32>,
    pub notes_after: Vec<f32>,
    pub boundary_ms: Vec<f32>,
    /// Voicing changed by the render, away from voicing edges.
    pub vuv_flips: usize,
    /// ... anywhere.
    pub vuv_flips_raw: usize,
    pub vuv_frames: usize,
    pub vibrato: Vec<(f32, f32)>,
    pub slips: usize,
    pub seconds: f32,
}

fn median_of(mut v: Vec<f32>) -> Option<f32> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

/// Vibrato rate (Hz) of a held note: the autocorrelation peak within 3–9 Hz of the
/// pitch with its straight-line trend (drift) removed. None without a clear vibrato.
fn vibrato_rate(seg: &[f32], rate: f32) -> Option<f32> {
    let n = seg.len();
    if n < (0.3 * rate) as usize || seg.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let mx = (n - 1) as f32 / 2.0;
    let my = seg.iter().sum::<f32>() / n as f32;
    let (sxy, sxx) =
        seg.iter().enumerate().fold((0f32, 0f32), |(a, b), (i, &y)| (a + (i as f32 - mx) * (y - my), b + (i as f32 - mx).powi(2)));
    let k = sxy / sxx.max(1e-6);
    let r: Vec<f32> = seg.iter().enumerate().map(|(i, &y)| y - my - k * (i as f32 - mx)).collect();
    let e0: f32 = r.iter().map(|x| x * x).sum();
    if (e0 / n as f32).sqrt() < 0.06 {
        return None;
    }
    let ac = |lag: usize| -> f32 { r.iter().zip(&r[lag..]).map(|(a, b)| a * b).sum::<f32>() / e0 * n as f32 / (n - lag) as f32 };
    let (lo, hi) = ((rate / 9.0) as usize, ((rate / 3.0) as usize).min(n / 2));
    let best = (lo..=hi).max_by(|&a, &b| ac(a).total_cmp(&ac(b)))?;
    if ac(best) < 0.3 {
        return None;
    }
    // parabolic peak interpolation
    let (y0, y1, y2) = (ac(best - 1), ac(best), ac((best + 1).min(n - 1)));
    let den = y0 - 2.0 * y1 + y2;
    let off = if den.abs() > 1e-6 { (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5) } else { 0.0 };
    Some(rate / (best as f32 + off))
}

pub fn run(c: &Case, p: &RefTune, est: Option<&dyn F0Estimator>) -> (Outcome, Audio) {
    let t0 = Instant::now();
    let (out, t, ta) = tune_audio(c.take, c.reference, p, est);
    let seconds = t0.elapsed().as_secs_f32();
    let oa: PitchAnalysis = analyze_with(&mono_of(&out), out.sr as f32, est);
    let rate = ta.rate;
    let n = ta.curve.len().min(oa.curve.len());
    let slip: Vec<bool> = (0..n)
        .map(|f| matches!((ta.curve[f], c.sung.get(f)), (a, Some(&s)) if a.is_finite() && s.is_finite() && (a - s).abs() > 1.0))
        .collect();
    let (mut frames_before, mut frames_after) = (vec![], vec![]);
    for (f, &slipped) in slip.iter().enumerate() {
        let Some(&w) = c.truth.get(f) else { break };
        if !w.is_finite() || c.edge.get(f).copied().unwrap_or(true) || slipped {
            continue;
        }
        if ta.curve[f].is_finite() {
            frames_before.push((ta.curve[f] - w).abs() * 100.0);
        }
        if oa.curve[f].is_finite() {
            frames_after.push((oa.curve[f] - w).abs() * 100.0);
        }
    }
    let (mut notes_before, mut notes_after, mut vibrato) = (vec![], vec![], vec![]);
    for &(a, b, m) in &c.notes {
        let (fa, fb) = (((a + 0.05) * rate as f64) as usize, (((b - 0.03) * rate as f64) as usize).min(n));
        if fb <= fa + 4 {
            continue;
        }
        let pick = |curve: &[f32]| median_of((fa..fb).filter(|&f| !slip[f]).map(|f| curve[f]).collect());
        if let (Some(x), Some(y)) = (pick(&ta.curve), pick(&oa.curve)) {
            notes_before.push((x - m) * 100.0);
            notes_after.push((y - m) * 100.0);
        }
        if b - a > 0.6 {
            let (va, vb) = (((a + 0.25) * rate as f64) as usize, (((b - 0.05) * rate as f64) as usize).min(n));
            if vb <= va {
                continue;
            }
            if let (Some(x), Some(y)) = (vibrato_rate(&ta.curve[va..vb], rate), vibrato_rate(&oa.curve[va..vb], rate)) {
                vibrato.push((x, y));
            }
        }
    }
    let boundary_ms = c
        .onsets
        .iter()
        .filter_map(|&(a, b)| {
            let i = ((a * rate as f64).round() as usize).checked_sub(t.start)?;
            let got = *t.ref_time.get(i)?;
            got.is_finite().then(|| ((got as f64 - b).abs() * 1000.0) as f32)
        })
        .collect();
    // a voicing edge moving by a frame or two is timing, not corruption: frames within 10 ms
    // of an edge of the take's voicing are counted apart
    let flip = |f: usize| ta.curve[f].is_nan() != oa.curve[f].is_nan();
    let tol = (0.01 * rate).ceil() as usize;
    let edge = |f: usize| (f.saturating_sub(tol)..(f + tol).min(n - 1)).any(|g| ta.curve[g].is_nan() != ta.curve[g + 1].is_nan());
    let vuv_flips_raw = (0..n).filter(|&f| flip(f)).count();
    let vuv_flips = (0..n).filter(|&f| flip(f) && !edge(f)).count();
    let untouched = out.content().iter().zip(c.take.content()).all(|(a, b)| (a - b).abs() < 1e-4);
    let o = Outcome {
        name: c.name.clone(),
        group: c.group.clone(),
        expect_rejection: c.expect_rejection,
        corrected: t.reason.is_none(),
        untouched,
        score: t.score,
        octave: t.octave,
        reason: t.reason.clone(),
        frames_before,
        frames_after,
        notes_before,
        notes_after,
        boundary_ms,
        vuv_flips,
        vuv_flips_raw,
        vuv_frames: n,
        vibrato,
        slips: slip.iter().filter(|s| **s).count(),
        seconds,
    };
    (o, out)
}

pub fn quantile(v: &[f32], q: f32) -> f32 {
    wave_engine::singer::quantile(v, q)
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        f32::NAN
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    }
}

/// Aggregate statistics of a set of outcomes, with the MVP thresholds checked.
pub fn summarize(label: &str, os: &[&Outcome]) -> Value {
    let accepted: Vec<&&Outcome> = os.iter().filter(|o| !o.expect_rejection).collect();
    let rejected: Vec<&&Outcome> = os.iter().filter(|o| o.expect_rejection).collect();
    let cat = |f: fn(&Outcome) -> &Vec<f32>| -> Vec<f32> { accepted.iter().flat_map(|o| f(o).iter().copied()).collect() };
    let (fb, fa) = (cat(|o| &o.frames_before), cat(|o| &o.frames_after));
    let (nb, na) = (cat(|o| &o.notes_before), cat(|o| &o.notes_after));
    let bms = cat(|o| &o.boundary_ms);
    let octave_err = |v: &[f32]| v.iter().filter(|e| e.abs() > 600.0).count() as f32 / v.len().max(1) as f32;
    let note_err = |v: &[f32]| v.iter().filter(|e| e.abs() > 50.0 && e.abs() <= 600.0).count() as f32 / v.len().max(1) as f32;
    let (mae_b, mae_a) = (mean(&fb), mean(&fa));
    let (nmae_b, nmae_a) = (mean(&nb.iter().map(|x| x.abs()).collect::<Vec<_>>()), mean(&na.iter().map(|x| x.abs()).collect::<Vec<_>>()));
    let flips: usize = accepted.iter().map(|o| o.vuv_flips).sum();
    let flips_raw: usize = accepted.iter().map(|o| o.vuv_flips_raw).sum();
    let frames: usize = accepted.iter().map(|o| o.vuv_frames).sum();
    let vib: Vec<f32> = accepted.iter().flat_map(|o| o.vibrato.iter().map(|(a, b)| (a - b).abs())).collect();
    let corrected = accepted.iter().filter(|o| o.corrected).count();
    let held_back = rejected.iter().filter(|o| !o.corrected && o.untouched).count();
    let has_frames = !fa.is_empty();
    let mut checks = vec![];
    let mut check = |name: &str, value: f32, limit: &str, ok: bool| {
        checks.push(json!({ "criterion": name, "value": value, "limit": limit, "pass": ok }))
    };
    if has_frames {
        check("frame error median (cents)", quantile(&fa, 0.5), "< 20", quantile(&fa, 0.5) < 20.0);
        check("frame error P90 (cents)", quantile(&fa, 0.9), "< 50", quantile(&fa, 0.9) < 50.0);
        check("MAE improvement", 1.0 - mae_a / mae_b, ">= 50 %", 1.0 - mae_a / mae_b >= 0.5);
    } else if !na.is_empty() {
        check(
            "note error median (cents)",
            quantile(&na.iter().map(|x| x.abs()).collect::<Vec<_>>(), 0.5),
            "< 20",
            quantile(&na.iter().map(|x| x.abs()).collect::<Vec<_>>(), 0.5) < 20.0,
        );
        check("note MAE improvement", 1.0 - nmae_a / nmae_b, ">= 50 %", 1.0 - nmae_a / nmae_b >= 0.5);
    }
    if !na.is_empty() {
        check("note errors (>50 ct)", note_err(&na), "< 2 %", note_err(&na) < 0.02);
        check("octave errors", octave_err(&na), "< 2 %", octave_err(&na) < 0.02);
    }
    if !bms.is_empty() {
        check("boundary error median (ms)", quantile(&bms, 0.5), "< 60", quantile(&bms, 0.5) < 60.0);
    }
    if frames > 0 {
        check("V/UV corruption (off edges ±10 ms)", flips as f32 / frames as f32, "< 1 %", (flips as f32) < frames as f32 * 0.01);
    }
    if !vib.is_empty() {
        check("vibrato rate change P90 (Hz)", quantile(&vib, 0.9), "< 0.5", quantile(&vib, 0.9) < 0.5);
    }
    if !rejected.is_empty() {
        check("low confidence → no correction", held_back as f32 / rejected.len() as f32, "100 %", held_back == rejected.len());
    }
    json!({
        "label": label,
        "cases": os.len(),
        "accepted": format!("{corrected}/{}", accepted.len()),
        "frames": { "count": fa.len(), "medianBefore": quantile(&fb, 0.5), "median": quantile(&fa, 0.5), "p90": quantile(&fa, 0.9), "maeBefore": mae_b, "mae": mae_a },
        "notes": { "count": na.len(), "maeBefore": nmae_b, "mae": nmae_a, "noteErrors": note_err(&na), "octaveErrors": octave_err(&na), "noteErrorsBefore": note_err(&nb) },
        "boundaryMs": { "count": bms.len(), "median": quantile(&bms, 0.5), "p90": quantile(&bms, 0.9) },
        "vuv": { "flips": flips, "flipsIncludingEdges": flips_raw, "frames": frames },
        "vibratoChangeHz": { "count": vib.len(), "median": quantile(&vib, 0.5), "p90": quantile(&vib, 0.9) },
        "rejection": format!("{held_back}/{}", rejected.len()),
        "detectorSlips": accepted.iter().map(|o| o.slips).sum::<usize>(),
        "secondsPerCase": mean(&os.iter().map(|o| o.seconds).collect::<Vec<_>>()),
        "checks": checks,
    })
}

pub fn outcome_json(o: &Outcome) -> Value {
    json!({
        "name": o.name, "group": o.group, "expectRejection": o.expect_rejection, "corrected": o.corrected, "untouched": o.untouched,
        "score": o.score, "octave": o.octave, "reason": o.reason,
        "frameMedian": quantile(&o.frames_after, 0.5), "frameMedianBefore": quantile(&o.frames_before, 0.5),
        "noteMae": mean(&o.notes_after.iter().map(|x| x.abs()).collect::<Vec<_>>()),
        "boundaryMedianMs": quantile(&o.boundary_ms, 0.5), "vuvFlips": o.vuv_flips, "vuvFlipsIncludingEdges": o.vuv_flips_raw, "slips": o.slips, "seconds": o.seconds,
    })
}

/// Human-readable table of a summary, on stderr.
pub fn print(s: &Value) {
    eprintln!(
        "\n== {} — {} cases, corrected {}, held back {}",
        s["label"].as_str().unwrap_or(""),
        s["cases"],
        s["accepted"].as_str().unwrap_or(""),
        s["rejection"].as_str().unwrap_or("")
    );
    for c in s["checks"].as_array().into_iter().flatten() {
        let v = c["value"].as_f64().unwrap_or(f64::NAN);
        let shown = if c["limit"].as_str().is_some_and(|l| l.contains('%')) { format!("{:.2} %", v * 100.0) } else { format!("{v:.2}") };
        eprintln!(
            "  {} {:<34} {:>10}   (limit {})",
            if c["pass"].as_bool() == Some(true) { "PASS" } else { "FAIL" },
            c["criterion"].as_str().unwrap_or(""),
            shown,
            c["limit"].as_str().unwrap_or("")
        );
    }
}

pub fn mono_audio(mono: &[f32], sr: u32) -> Audio {
    Audio::from_stereo(mono.iter().flat_map(|&x| [x, x]).collect(), sr, 1)
}
