//! The pitch editor for agents: what a vocal clip sings and how it is edited
//! ([`inspect`]), how it relates to another performance ([`compare`]) and a conservative,
//! explained correction proposal ([`suggest`]). All read-only and deterministic; edits go
//! through the `editTune` op (`crate::tune_edit`), the same one the pitch editor sends.
//!
//! The split is deliberate: these functions measure (pitch, stability, alignment, interval)
//! and say how sure they are; deciding what a singer meant stays with the caller. `suggest`
//! is one such caller, kept small and cautious: it leaves a note alone whenever the evidence
//! is weak or the difference looks intended.

use serde::Serialize;

use crate::content::ContentFeatures;
use crate::model::{ClipTune, NoteEdit};
use crate::pitch::{scale_mask, snap, PitchAnalysis};
use crate::reference::{align_with, Alignment, Performance, PitchTarget, LOOSE, MIN_SCORE};

pub const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// "A3" for MIDI 57 (C4 = 60, as the editor labels keys).
pub fn note_name(m: f32) -> String {
    let k = m.round() as i32;
    format!("{}{}", NOTE_NAMES[k.rem_euclid(12) as usize], k.div_euclid(12) - 1)
}

fn median(mut v: Vec<f32>) -> Option<f32> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

fn quantile(mut v: Vec<f32>, q: f32) -> Option<f32> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[((v.len() - 1) as f32 * q).round() as usize])
}

fn r2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}
fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn frames(rate: f32, len: usize, a: f64, b: f64) -> (usize, usize) {
    let f0 = ((a * rate as f64).round().max(0.0) as usize).min(len);
    let f1 = ((b * rate as f64).round().max(0.0) as usize).min(len).max(f0);
    (f0, f1)
}

/// Vibrato of a held stretch of pitch (MIDI per frame at `rate`): the autocorrelation peak within
/// 3–9 Hz after the straight-line trend (drift) is removed. `(rate Hz, depth cents)`, where depth
/// is the peak deviation (√2 · RMS). None without a clear periodic movement.
pub fn vibrato(seg: &[f32], rate: f32) -> Option<(f32, f32)> {
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
    let rms = (e0 / n as f32).sqrt();
    if rms < 0.06 {
        return None;
    }
    let ac = |lag: usize| -> f32 { r.iter().zip(&r[lag..]).map(|(a, b)| a * b).sum::<f32>() / e0 * n as f32 / (n - lag) as f32 };
    let (lo, hi) = (((rate / 9.0) as usize).max(2), ((rate / 3.0) as usize).min(n / 2));
    if hi <= lo {
        return None;
    }
    let best = (lo..=hi).max_by(|&a, &b| ac(a).total_cmp(&ac(b)))?;
    if ac(best) < 0.3 {
        return None;
    }
    let (y0, y1, y2) = (ac(best - 1), ac(best), ac((best + 1).min(n - 1)));
    let den = y0 - 2.0 * y1 + y2;
    let off = if den.abs() > 1e-6 { (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5) } else { 0.0 };
    Some((rate / (best as f32 + off), rms * std::f32::consts::SQRT_2 * 100.0))
}

// ------------------------------------------------------------------ inspection

/// What a note is, for deciding whether to touch it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NoteKind {
    /// A held pitch: its centre is a meaningful note.
    Stable,
    /// Held with a clear vibrato: the centre is meaningful, the movement is the singer's.
    Vibrato,
    /// A slide or scoop through pitches: no single intended pitch.
    Glide,
    /// Under 80 ms: an ornament or transition rather than a note.
    Short,
    /// The detector is unsure (breathy, noisy or mostly unvoiced).
    Unsure,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteStats {
    /// Index in the clip's segmentation: what `editTune` addresses.
    pub i: usize,
    /// Seconds of the source.
    pub start: f64,
    pub end: f64,
    pub kind: NoteKind,
    /// Median sung pitch (MIDI, fractional), as recorded; None if nothing is voiced.
    pub sung: Option<f32>,
    /// The nearest equal-tempered note (A4 = 440 Hz) and the distance to it, cents (+ = sharp).
    pub nearest: Option<String>,
    pub cents: Option<f32>,
    /// Median pitch it will sound at with the current edits (MIDI).
    pub out: Option<f32>,
    /// The note's current edits.
    pub shift: f32,
    pub drift: f32,
    pub formant: f32,
    pub gain: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    /// Mean detector confidence over the note (0..1) and the share of its frames that are voiced.
    pub confidence: f32,
    pub voiced: f32,
    /// Peak level, dBFS.
    pub level_db: f32,
    /// Pitch change across the held middle of the note (its central 60 %) from a straight-line
    /// fit, cents: a slide through the note shows here, a scoop into it does not.
    pub slope_cents: f32,
    /// Spread of the pitch around that line, cents (P10–P90 / 2).
    pub spread_cents: f32,
    /// `[rate Hz, depth cents]` when there is a clear vibrato.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vibrato: Option<[f32; 2]>,
    /// Median pitch of the note's second half minus its first half (central 80 %), cents: a
    /// step here is two pitches sung as one note (legato), which one shift cannot correct.
    pub step_cents: f32,
}

/// Per-note statistics of `notes` over the analysis `a`, with `out` the curve the render will
/// sing (`tune_render::edited_curve`).
pub fn note_stats(a: &PitchAnalysis, notes: &[NoteEdit], out: &[f32]) -> Vec<NoteStats> {
    let rate = a.rate;
    notes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let (f0, f1) = frames(rate, a.curve.len(), n.start, n.end);
            let seg = &a.curve[f0..f1];
            let voiced_n = seg.iter().filter(|x| x.is_finite()).count();
            let voiced = voiced_n as f32 / (f1 - f0).max(1) as f32;
            let confidence = if f1 > f0 { a.confidence[f0..f1].iter().sum::<f32>() / (f1 - f0) as f32 } else { 0.0 };
            let sung = median(seg.to_vec());
            let level = a.levels.get(f0..f1).map(|l| l.iter().fold(0f32, |m, x| m.max(*x))).unwrap_or(0.0);
            let outm = median(out.get(f0..f1).map(|o| o.to_vec()).unwrap_or_default());
            // slope and spread over the held middle of the note (its central 60 %): a scoop into
            // the note or a fall off its end is a transition, the middle says whether it is a slide
            let len = f1 - f0;
            let (m0, m1) = (f0 + len / 5, f1 - len / 5);
            let pts: Vec<(f32, f32)> =
                (m0..m1).filter_map(|f| a.curve.get(f).filter(|x| x.is_finite()).map(|&x| (f as f32 / rate, x))).collect();
            let (slope, spread) = if pts.len() >= 4 {
                let (mx, my) =
                    (pts.iter().map(|p| p.0).sum::<f32>() / pts.len() as f32, pts.iter().map(|p| p.1).sum::<f32>() / pts.len() as f32);
                let sxx: f32 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
                let k = if sxx > 1e-9 { pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f32>() / sxx } else { 0.0 };
                let res: Vec<f32> = pts.iter().map(|p| p.1 - my - k * (p.0 - mx)).collect();
                let span = pts[pts.len() - 1].0 - pts[0].0;
                let sp = (quantile(res.clone(), 0.9).unwrap_or(0.0) - quantile(res, 0.1).unwrap_or(0.0)) / 2.0;
                (k * span * 100.0, sp * 100.0)
            } else {
                (0.0, 0.0)
            };
            // vibrato over the held part (from 150 ms in, to 50 ms before the end)
            let (va, vb) = (f0 + (0.15 * rate) as usize, f1.saturating_sub((0.05 * rate) as usize));
            let vib = if vb > va { vibrato(&a.curve[va..vb], rate) } else { None };
            let (s0, s1) = (f0 + len / 10, f1 - len / 10);
            let mid = (s0 + s1) / 2;
            let step = match (median(a.curve[s0..mid].to_vec()), median(a.curve[mid..s1.max(mid)].to_vec())) {
                (Some(x), Some(y)) => (y - x) * 100.0,
                _ => 0.0,
            };
            let dur = n.end - n.start;
            let kind = if dur < 0.08 {
                NoteKind::Short
            } else if voiced < 0.5 || sung.is_none() || confidence < 0.35 {
                NoteKind::Unsure
            } else if vib.is_some_and(|v| v.1 >= 15.0) {
                NoteKind::Vibrato
            } else if slope.abs() >= 100.0 && slope.abs() > 2.5 * spread {
                NoteKind::Glide
            } else {
                NoteKind::Stable
            };
            NoteStats {
                i,
                start: n.start,
                end: n.end,
                kind,
                sung: sung.map(r2),
                nearest: sung.map(note_name),
                cents: sung.map(|s| r1((s - s.round()) * 100.0)),
                out: outm.map(r2),
                shift: n.shift,
                drift: n.drift,
                formant: n.formant,
                gain: n.gain,
                speed: n.speed,
                confidence: r2(confidence),
                voiced: r2(voiced),
                level_db: r1(20.0 * level.max(1e-6).log10()),
                slope_cents: r1(slope),
                spread_cents: r1(spread),
                vibrato: vib.map(|v| [r2(v.0), r1(v.1)]),
                step_cents: r1(step),
            }
        })
        .collect()
}

/// The notes a clip is edited by: its own, else the analysis' segmentation (no edits).
pub fn clip_notes(tune: Option<&ClipTune>, a: &PitchAnalysis) -> (Vec<NoteEdit>, bool) {
    match tune.filter(|t| !t.notes.is_empty()) {
        Some(t) => (t.notes.clone(), true),
        None => (
            a.notes
                .iter()
                .map(|n| NoteEdit { start: n.start, end: n.end, shift: 0.0, drift: 0.0, formant: 0.0, gain: 0.0, speed: None })
                .collect(),
            false,
        ),
    }
}

/// Overall intonation of a set of notes: the median offset from equal temperament (a take
/// sung consistently sharp, or to an instrument tuned away from 440 Hz) and how far the
/// stable notes scatter around it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tuning {
    pub offset_cents: Option<f32>,
    pub spread_cents: Option<f32>,
    pub notes: usize,
}

pub fn tuning(stats: &[NoteStats]) -> Tuning {
    let c: Vec<f32> = stats.iter().filter(|s| matches!(s.kind, NoteKind::Stable | NoteKind::Vibrato)).filter_map(|s| s.cents).collect();
    let off = circular_cents(&c);
    let spread = off.and_then(|o| median(c.iter().map(|v| ((v - o + 50.0).rem_euclid(100.0) - 50.0).abs()).collect()));
    Tuning { offset_cents: off.map(r1), spread_cents: spread.map(r1), notes: c.len() }
}

/// Median of cents offsets that wrap at ±50 (centred on their circular mean first).
fn circular_cents(c: &[f32]) -> Option<f32> {
    let (sx, sy) = c.iter().fold((0f32, 0f32), |(x, y), v| {
        let a = v / 100.0 * std::f32::consts::TAU;
        (x + a.cos(), y + a.sin())
    });
    let mean = sy.atan2(sx) / std::f32::consts::TAU * 100.0;
    let m = median(c.iter().map(|v| mean + (v - mean + 50.0).rem_euclid(100.0) - 50.0).collect())?;
    Some((m + 50.0).rem_euclid(100.0) - 50.0)
}

/// Frame-level curves of `[f0, f1)`: sung pitch, detector confidence and the pitch it will sound
/// at (MIDI rounded to 1/100 semitone; null = unvoiced).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Frames {
    pub rate: f32,
    /// Seconds of the source of the first frame.
    pub start: f64,
    pub f0: Vec<Option<f32>>,
    pub confidence: Vec<f32>,
    pub out: Vec<Option<f32>>,
}

pub fn frames_of(a: &PitchAnalysis, out: &[f32], from: f64, to: f64) -> Frames {
    let (f0, f1) = frames(a.rate, a.curve.len(), from, to);
    let opt = |x: f32| x.is_finite().then(|| r2(x));
    Frames {
        rate: a.rate,
        start: f0 as f64 / a.rate as f64,
        f0: a.curve[f0..f1].iter().map(|&x| opt(x)).collect(),
        confidence: a.confidence[f0..f1].iter().map(|&x| r2(x)).collect(),
        out: (f0..f1).map(|f| out.get(f).copied().and_then(opt)).collect(),
    }
}

// ------------------------------------------------------------------ comparison

/// One interval candidate between take and reference.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Interval {
    /// Semitones the take sits above the reference (−36..36), the nearest to `cents`.
    pub semitones: i32,
    /// The measured offset, cents: the interval plus the difference of the two singers' tunings
    /// (a fifth sung 20 ct flatter reads 680).
    pub cents: f32,
    /// Share of the compared frames within ±50 cents of it.
    pub support: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Relation {
    /// One take note, one reference note.
    Match,
    /// One take note over several reference notes (the reference ornaments or re-articulates).
    Merged,
    /// Several take notes over one reference note.
    Split,
    /// Several on both sides.
    Complex,
    /// Nothing in the reference is aligned with confidence here (an ad-lib, or not trusted).
    Extra,
}

/// How one take note relates to the reference.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteMatch {
    pub i: usize,
    pub relation: Relation,
    /// Reference notes covering it (indices into `Comparison::reference_notes`), most first.
    pub reference: Vec<usize>,
    /// The main reference note's pitch moved by the interval, MIDI: where the reference puts
    /// this note in the take's register.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<f32>,
    /// Take − expected, cents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_cents: Option<f32>,
    /// The take note starts this much later than the reference note maps to, ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onset_ms: Option<f32>,
    /// Median alignment confidence over the note, 0..1.
    pub confidence: f32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefNote {
    /// Seconds of the reference.
    pub start: f64,
    pub end: f64,
    /// MIDI as the reference sings it.
    pub pitch: f32,
    /// Seconds of the take it maps to (None: no confident mapping).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    /// Take notes it covers.
    pub take: Vec<usize>,
    /// Shorter than 150 ms inside a run the take sings as one note: likely an ornament.
    pub ornament: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    /// The reference sings the same melody: the alignment is trusted (`aligned`) and at least
    /// half of the confidently matched notes agree within 50 ct once the interval is applied.
    /// Only a related reference is used by [`suggest`].
    pub related: bool,
    /// The two performances could be aligned in time (score ≥ 0.3): the same words or material,
    /// whatever the notes (a harmony part aligns but is not related).
    pub aligned: bool,
    /// Median alignment confidence over the take's sung frames (Tune to Reference's score).
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Best interval first; a close second means the interval is ambiguous.
    pub intervals: Vec<Interval>,
    pub ambiguous_interval: bool,
    /// Share of the take's sung time aligned with confidence.
    pub coverage: f32,
    /// Of the confidently matched notes: share within 50 cents of the reference after the
    /// interval (same melody), and how well consecutive intervals agree (−1..1).
    pub same_notes: Option<f32>,
    pub contour: Option<f32>,
    pub counts: Counts,
    pub notes: Vec<NoteMatch>,
    pub reference_notes: Vec<RefNote>,
    /// Stretches of the take (seconds) sung but not confidently aligned.
    pub unmatched: Vec<[f64; 2]>,
    /// Median timing offset of matched onsets (take later = +), ms, and its spread (P90 |·|).
    pub timing_ms: Option<f32>,
    pub timing_p90_ms: Option<f32>,
    /// The reference's own offset from A440 equal temperament over its held notes, cents (a
    /// recording tuned to 442 Hz reads about +8).
    pub reference_tuning_cents: Option<f32>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub take_notes: usize,
    pub reference_notes: usize,
    pub matched: usize,
    pub merged: usize,
    pub split: usize,
    pub complex: usize,
    pub extra: usize,
    /// Reference notes inside the aligned stretch with no take note (not sung).
    pub missing: usize,
}

/// Interval candidates for take − reference from per-frame differences (semitones): the modes
/// of their density (σ 30 ct), at least 70 ct apart, best first. Two singers rarely share a
/// tuning, so the mode is found on a fine grid and only then named by its nearest semitone.
fn intervals(diffs: &[f32]) -> Vec<Interval> {
    if diffs.is_empty() {
        return vec![];
    }
    let grid: Vec<f32> = (0..=1460).map(|k| -36.5 + k as f32 * 0.05).collect();
    let dens: Vec<f32> =
        grid.iter().map(|&g| diffs.iter().map(|d| (-(d - g).powi(2) / (2.0 * 0.3f32.powi(2))).exp()).sum::<f32>()).collect();
    let mut peaks: Vec<(f32, f32)> = (1..grid.len() - 1)
        .filter(|&k| dens[k] > 0.0 && dens[k] >= dens[k - 1] && dens[k] > dens[k + 1])
        .map(|k| (grid[k], dens[k]))
        .collect();
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut picked: Vec<f32> = vec![];
    for (m, _) in peaks {
        if picked.iter().all(|p| (p - m).abs() >= 0.7) {
            picked.push(m);
        }
        if picked.len() == 3 {
            break;
        }
    }
    picked
        .into_iter()
        .map(|m| {
            let near: Vec<f32> = diffs.iter().copied().filter(|d| (d - m).abs() < 0.5).collect();
            let c = median(near.clone()).unwrap_or(m);
            Interval { semitones: c.round() as i32, cents: r1(c * 100.0), support: r2(near.len() as f32 / diffs.len() as f32) }
        })
        .collect()
}

fn diffs_of(take: &PitchAnalysis, al: &Alignment, shift: f32) -> Vec<f32> {
    let pick = |min: f32| -> Vec<f32> {
        (0..al.ref_time.len())
            .filter(|&i| al.confidence[i] >= min)
            .filter_map(|i| {
                let u = *take.curve.get(al.start + i)?;
                let r = al.reference_f0[i] + shift;
                (u.is_finite() && r.is_finite()).then_some(u - r)
            })
            .collect()
    };
    let d = pick(0.4);
    if d.len() >= 40 {
        d
    } else {
        pick(0.0)
    }
}

/// Compares a take (its pitch, content features, the clip's region and note spans) with another
/// performance. Alignment is Tune to Reference's (content first, pitch shape second), done again
/// with the reference moved by a non-octave interval when that is what the first pass finds, so
/// a reference in another key aligns as well as one in another octave.
pub fn compare(
    take: &PitchAnalysis,
    take_content: &ContentFeatures,
    region: (f64, f64),
    spans: &[(f64, f64)],
    reference: &PitchAnalysis,
    ref_content: &ContentFeatures,
) -> Comparison {
    let tp = Performance { pitch: take, content: take_content };
    let mut al = align_with(&tp, region, &Performance { pitch: reference, content: ref_content }, LOOSE);
    // semitones the aligned reference curve was moved by (its `reference_f0` includes it)
    let mut moved_by = 0.0f32;
    let first = intervals(&diffs_of(take, &al, 0.0));
    if let Some(best) = first.first() {
        let pc = best.semitones - 12 * (best.semitones as f32 / 12.0).round() as i32;
        if pc != 0 {
            let mut moved = reference.clone();
            moved.curve.iter_mut().for_each(|x| *x += pc as f32);
            let al2 = align_with(&tp, region, &Performance { pitch: &moved, content: ref_content }, LOOSE);
            if al2.score > al.score {
                al = al2;
                moved_by = pc as f32;
            }
        }
    }
    // intervals relative to the reference as recorded
    let iv = intervals(&diffs_of(take, &al, -moved_by));
    let ambiguous_interval = match (iv.first(), iv.get(1)) {
        (Some(a), Some(b)) => (b.support >= 0.6 * a.support && a.semitones != b.semitones) || a.support < 0.3,
        (Some(a), None) => a.support < 0.3,
        (None, _) => true,
    };
    // the measured interval, tuning difference included: `expected` is in the take's own frame
    let interval = iv.first().map(|i| i.cents / 100.0).unwrap_or(0.0);
    let rate = take.rate;
    let n = al.ref_time.len();
    let aligned = al.score >= MIN_SCORE && al.reason.is_none();
    let related = aligned;
    // reference notes on the take's time axis
    let ref_note_of = |t: f32| -> Option<usize> {
        if !t.is_finite() {
            return None;
        }
        let t = t as f64;
        reference.notes.iter().position(|nt| t >= nt.start && t < nt.end)
    };
    let take_frame = |f: usize| -> Option<usize> { f.checked_sub(al.start).filter(|&i| i < n) };
    let mut ref_cover: Vec<Vec<(usize, usize)>> = vec![vec![]; reference.notes.len()]; // (take note, frames)
    let mut ref_span: Vec<Option<(f64, f64)>> = vec![None; reference.notes.len()];
    let mut ref_frames = vec![0usize; reference.notes.len()];
    let trusted = |i: usize| related && al.confidence[i] >= 0.3;
    for i in 0..n {
        if !trusted(i) {
            continue;
        }
        if let Some(k) = ref_note_of(al.ref_time[i]) {
            ref_frames[k] += 1;
            let t = (al.start + i) as f64 / rate as f64;
            let s = ref_span[k].get_or_insert((t, t));
            s.0 = s.0.min(t);
            s.1 = s.1.max(t + 1.0 / rate as f64);
        }
    }
    let mut out_notes = vec![];
    let mut onsets = vec![];
    let mut confident_diffs = vec![];
    let mut pairs: Vec<(f32, f32)> = vec![]; // (take pitch, expected) of confident 1:1 notes, in order
    for (ti, &(a, b)) in spans.iter().enumerate() {
        let (f0, f1) = frames(rate, take.curve.len(), a, b);
        let mut count: Vec<(usize, usize)> = vec![];
        let mut voiced = 0usize;
        let mut confs = vec![];
        for f in f0..f1 {
            if !take.curve[f].is_finite() {
                continue;
            }
            voiced += 1;
            let Some(i) = take_frame(f) else { continue };
            confs.push(al.confidence[i]);
            if !trusted(i) {
                continue;
            }
            if let Some(k) = ref_note_of(al.ref_time[i]) {
                match count.iter_mut().find(|c| c.0 == k) {
                    Some(c) => c.1 += 1,
                    None => count.push((k, 1)),
                }
            }
        }
        let confidence = r2(median(confs).unwrap_or(0.0));
        // a reference note belongs to this note when it covers ≥ 30 % of the note's sung frames, or
        // lies mostly (≥ 50 %) inside it; and at least 25 ms either way
        let least = (0.025 * rate) as usize;
        count.retain(|c| c.1 >= least && (c.1 as f32 >= 0.3 * voiced as f32 || c.1 as f32 >= 0.5 * ref_frames[c.0] as f32));
        count.sort_by_key(|x| std::cmp::Reverse(x.1));
        for &(k, c) in &count {
            ref_cover[k].push((ti, c));
        }
        let refs: Vec<usize> = count.iter().map(|c| c.0).collect();
        let sung = median(take.curve[f0..f1].to_vec());
        let expected = refs.first().map(|&k| reference.notes[k].pitch + interval);
        let diff = match (sung, expected) {
            (Some(s), Some(e)) => Some(r1((s - e) * 100.0)),
            _ => None,
        };
        let onset = refs.first().and_then(|&k| ref_span[k]).map(|s| r1(((a - s.0) * 1000.0) as f32));
        out_notes.push(NoteMatch {
            i: ti,
            relation: if refs.is_empty() {
                Relation::Extra
            } else if refs.len() == 1 {
                Relation::Match
            } else {
                Relation::Merged
            },
            reference: refs,
            expected: expected.map(r2),
            diff_cents: diff,
            onset_ms: onset,
            confidence,
        });
    }
    // several take notes on one reference note: split (or complex when they also span others)
    for cov in &ref_cover {
        if cov.len() < 2 {
            continue;
        }
        for &(ti, _) in cov {
            let m = &mut out_notes[ti];
            m.relation = if m.reference.len() > 1 { Relation::Complex } else { Relation::Split };
        }
    }
    for m in &out_notes {
        if m.relation == Relation::Match {
            if let Some(o) = m.onset_ms {
                onsets.push(o);
            }
        }
        if m.confidence >= 0.45 && matches!(m.relation, Relation::Match | Relation::Split) {
            if let (Some(d), Some(e)) = (m.diff_cents, m.expected) {
                confident_diffs.push(d);
                pairs.push((e + d / 100.0, e));
            }
        }
    }
    let mut counts = Counts { take_notes: spans.len(), reference_notes: reference.notes.len(), ..Default::default() };
    for m in &out_notes {
        match m.relation {
            Relation::Match => counts.matched += 1,
            Relation::Merged => counts.merged += 1,
            Relation::Split => counts.split += 1,
            Relation::Complex => counts.complex += 1,
            Relation::Extra => counts.extra += 1,
        }
    }
    // reference notes inside the aligned stretch that no take note covers
    let (lo, hi) = (0..n).filter(|&i| trusted(i)).fold((f32::MAX, f32::MIN), |(lo, hi), i| {
        let t = al.ref_time[i];
        if t.is_finite() {
            (lo.min(t), hi.max(t))
        } else {
            (lo, hi)
        }
    });
    let reference_notes: Vec<RefNote> = reference
        .notes
        .iter()
        .enumerate()
        .map(|(k, nt)| {
            let take_notes: Vec<usize> = ref_cover[k].iter().map(|c| c.0).collect();
            let merged_run = take_notes.iter().any(|&ti| out_notes[ti].reference.len() > 1);
            RefNote {
                start: nt.start,
                end: nt.end,
                pitch: r2(nt.pitch),
                at: ref_span[k].map(|s| [s.0, s.1]),
                ornament: merged_run && nt.end - nt.start < 0.15,
                take: take_notes,
            }
        })
        .collect();
    counts.missing = reference_notes
        .iter()
        .filter(|r| r.take.is_empty() && (r.start as f32) >= lo && (r.end as f32) <= hi && r.end - r.start >= 0.08)
        .count();
    // unmatched sung stretches
    let mut unmatched = vec![];
    let mut s: Option<usize> = None;
    let (rf0, rf1) = frames(rate, take.curve.len(), region.0, region.1);
    for f in rf0..=rf1 {
        let low = f < rf1 && take.curve[f].is_finite() && take_frame(f).is_none_or(|i| !trusted(i) || al.confidence[i] < 0.2);
        match (low, s) {
            (true, None) => s = Some(f),
            (false, Some(a)) => {
                if f - a >= (0.1 * rate) as usize {
                    unmatched.push([a as f64 / rate as f64, f as f64 / rate as f64]);
                }
                s = None;
            }
            _ => {}
        }
    }
    let sung_frames = (rf0..rf1).filter(|&f| take.curve[f].is_finite()).count();
    let covered =
        (rf0..rf1).filter(|&f| take.curve[f].is_finite() && take_frame(f).is_some_and(|i| trusted(i) && al.confidence[i] >= 0.2)).count();
    let same_notes = (!confident_diffs.is_empty())
        .then(|| r2(confident_diffs.iter().filter(|d| d.abs() < 50.0).count() as f32 / confident_diffs.len() as f32));
    let contour = (pairs.len() >= 3).then(|| {
        let (dx, dy): (Vec<f32>, Vec<f32>) = pairs.windows(2).map(|w| (w[1].0 - w[0].0, w[1].1 - w[0].1)).unzip();
        let (mx, my) = (dx.iter().sum::<f32>() / dx.len() as f32, dy.iter().sum::<f32>() / dy.len() as f32);
        let cov: f32 = dx.iter().zip(&dy).map(|(a, b)| (a - mx) * (b - my)).sum();
        let (vx, vy): (f32, f32) = (dx.iter().map(|a| (a - mx).powi(2)).sum(), dy.iter().map(|b| (b - my).powi(2)).sum());
        if vx > 1e-6 && vy > 1e-6 {
            r2(cov / (vx * vy).sqrt())
        } else {
            0.0
        }
    });
    let held: Vec<f32> =
        reference.notes.iter().filter(|nt| nt.end - nt.start >= 0.15).map(|nt| (nt.pitch - nt.pitch.round()) * 100.0).collect();
    let reference_tuning_cents = (held.len() >= 3).then(|| circular_cents(&held)).flatten().map(r1);
    // the same material is not yet the same melody: most matched notes must agree
    let related = related && same_notes.is_some_and(|s| s >= 0.5);
    Comparison {
        related,
        aligned,
        reference_tuning_cents,
        score: r2(al.score),
        reason: al.reason.clone(),
        intervals: iv,
        ambiguous_interval,
        coverage: r2(covered as f32 / sung_frames.max(1) as f32),
        same_notes,
        contour,
        counts,
        timing_ms: median(onsets.clone()).map(r1),
        timing_p90_ms: quantile(onsets.iter().map(|x| x.abs()).collect(), 0.9).map(r1),
        notes: out_notes,
        reference_notes,
        unmatched,
    }
}

// ------------------------------------------------------------------ suggestions

/// Limits and context of [`suggest`].
#[derive(Clone, Debug)]
pub struct SuggestParams {
    /// Notes within this of their intended pitch are left alone, cents.
    pub min_cents: f32,
    /// Corrections larger than this are never proposed, cents. A take further than this from a
    /// confidently matched reference note is taken to sing a different note on purpose (a
    /// semitone or more off but itself in tune is what a deliberate variation looks like). Raise
    /// it when the take is known to be far off (up to 1200).
    pub max_cents: f32,
    /// Fraction of the way to the intended pitch (1 = all of it).
    pub strength: f32,
    /// Key (0 = C) and scale (`pitch::scale_mask`) when known; None = chromatic.
    pub key: Option<(u32, u32)>,
    /// Reference of the equal-tempered grid, cents off A440 (an instrument tuned to 442 Hz is
    /// +7.9; `wave analyze` measures a backing track's). None = A440. Not guessed from the
    /// reference vocal: an unaccompanied singer's own drift is not the song's tuning.
    pub tuning: Option<f32>,
}

impl Default for SuggestParams {
    fn default() -> Self {
        Self { min_cents: 15.0, max_cents: 60.0, strength: 1.0, key: None, tuning: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    Edit,
    Keep,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub i: usize,
    pub decision: Decision,
    /// Why, in a few words.
    pub reason: String,
    /// Sung median, the pitch it sounds at now, and the proposed one (MIDI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sung: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<f32>,
    /// Proposed change from `now`, cents, and the resulting note `shift`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_cents: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shift: Option<f32>,
    /// 0..1: how much the evidence agrees.
    pub confidence: f32,
    /// Where the intended pitch came from: "reference", "scale" or "nearest".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
}

fn grid(m: f32, tuning_ct: f32) -> f32 {
    let off = tuning_ct / 100.0;
    (m - off).round() + off
}

/// Proposes per-note corrections. `stats` are the clip's notes (with `out`, the pitch they
/// sound at now), `base_centers` the centres the note `shift`s are relative to
/// (`tune_render`'s note centre of the base curve), `cmp` an optional comparison with a
/// reference. Every note gets a decision and a reason; only `Edit`s change anything.
///
/// The rules, in order: notes that are short, unsure or slides are kept (no single intended
/// pitch); a reference that does not match the take (`related` false) changes nothing at all;
/// with a related reference that confidently matches the note, the intended pitch is
/// the reference's note in the take's register — unless the take sings a clearly different
/// note (further than `max_cents`), which is taken as intended; without one, the nearest
/// note of the key (or the nearest semitone), unless the take sits near the middle between
/// two notes, where the intention is unknowable. Within `min_cents` nothing is done.
pub fn suggest(stats: &[NoteStats], base_centers: &[Option<f32>], cmp: Option<&Comparison>, p: &SuggestParams) -> Vec<Suggestion> {
    // a reference that does not match this take is no evidence for anything: change nothing
    if let Some(c) = cmp.filter(|c| !c.related) {
        let why = format!("the reference does not match this take (score {:.2}): nothing changed", c.score);
        return stats
            .iter()
            .map(|s| Suggestion {
                i: s.i,
                decision: Decision::Keep,
                reason: why.clone(),
                sung: s.sung,
                now: s.out,
                target: None,
                change_cents: None,
                shift: None,
                confidence: 0.0,
                source: None,
            })
            .collect();
    }
    let reference = cmp;
    let tuning_ct = p.tuning.unwrap_or(0.0);
    let mask = p.key.map(|(k, s)| (k, scale_mask(s)));
    stats
        .iter()
        .map(|s| {
            let keep = |reason: String, conf: f32| Suggestion {
                i: s.i,
                decision: Decision::Keep,
                reason,
                sung: s.sung,
                now: s.out,
                target: None,
                change_cents: None,
                shift: None,
                confidence: r2(conf),
                source: None,
            };
            let (Some(sung), Some(now)) = (s.sung, s.out) else {
                return keep("nothing sung to measure".into(), 0.0);
            };
            match s.kind {
                NoteKind::Short => return keep("under 80 ms: an ornament or transition, left as sung".into(), 0.0),
                NoteKind::Unsure => {
                    return keep(format!("pitch unsure (confidence {:.2}, voiced {:.0} %)", s.confidence, s.voiced * 100.0), 0.0)
                }
                NoteKind::Glide => return keep(format!("a slide ({:+.0} ct across the note): left as sung", s.slope_cents), 0.0),
                _ => {}
            }
            if s.kind != NoteKind::Vibrato && s.step_cents.abs() >= 25.0 {
                return keep(format!("two pitches in one note (a {:+.0} ct step): split it first to correct either", s.step_cents), 0.0);
            }
            let det = s.confidence.clamp(0.0, 1.0);
            // 1. the reference, where it confidently says which note this is
            let m = reference
                .and_then(|c| c.notes.get(s.i))
                .filter(|m| m.confidence >= 0.45 && matches!(m.relation, Relation::Match | Relation::Split | Relation::Merged));
            let (target, source, conf) = if let (Some(m), Some(exp)) = (m, m.and_then(|m| m.expected)) {
                let want = grid(exp, tuning_ct);
                let diff = (sung - want) * 100.0;
                if diff.abs() > p.max_cents {
                    let near = (sung - grid(sung, tuning_ct)).abs() * 100.0;
                    let why = if near < 30.0 {
                        format!(
                            "sings {} where the reference has {} ({diff:+.0} ct): a different note, taken as intended",
                            note_name(sung),
                            note_name(want)
                        )
                    } else {
                        format!("{diff:+.0} ct from the reference's note and between notes: intention unclear")
                    };
                    return keep(why, m.confidence * det);
                }
                (want, "reference", m.confidence * det)
            } else if let Some((k, mk)) = mask {
                let want = snap(sung - tuning_ct / 100.0, k, mk) + tuning_ct / 100.0;
                let diff = (sung - want) * 100.0;
                if diff.abs() > 65.0 {
                    return keep(
                        format!("{diff:+.0} ct from the nearest note of the key: maybe a chromatic note, left as sung"),
                        det * 0.5,
                    );
                }
                let c = det * (1.0 - (diff.abs() / 100.0)).clamp(0.0, 1.0);
                (want, "scale", c)
            } else {
                let want = grid(sung, tuning_ct);
                let diff = (sung - want) * 100.0;
                if diff.abs() > 35.0 {
                    return keep(
                        format!(
                            "{diff:+.0} ct: near the middle between two notes, the intended one is unknowable without a reference or key"
                        ),
                        det * 0.3,
                    );
                }
                (want, "nearest", det * (1.0 - diff.abs() / 50.0).clamp(0.0, 1.0))
            };
            let change = (target - now) * 100.0 * p.strength.clamp(0.0, 1.0);
            let from_sung = (target - sung) * 100.0;
            if from_sung.abs() > p.max_cents {
                return keep(format!("the correction ({from_sung:+.0} ct) is over the {:.0} ct limit", p.max_cents), conf);
            }
            if change.abs() < p.min_cents {
                return keep(
                    format!("in tune: {:+.0} ct from {} (within {:.0} ct)", (now - target) * 100.0, note_name(target), p.min_cents),
                    conf,
                );
            }
            let base = base_centers.get(s.i).copied().flatten().unwrap_or(sung);
            let shift = ((now + change / 100.0 - base) * 100.0).round();
            Suggestion {
                i: s.i,
                decision: Decision::Edit,
                reason: format!("{:+.0} ct from {} ({source})", (now - target) * 100.0, note_name(target)),
                sung: Some(sung),
                now: Some(now),
                target: Some(r2(target)),
                change_cents: Some(r1(change)),
                shift: Some(shift),
                confidence: r2(conf),
                source: Some(source),
            }
        })
        .collect()
}

/// Note centres of the base curve (the reference target where there is one, else the sung
/// pitch): what each note's `shift` moves (`tune_render`'s `note_center`).
pub fn base_centers(a: &PitchAnalysis, notes: &[NoteEdit], target: Option<&PitchTarget>) -> Vec<Option<f32>> {
    let base: Vec<f32> = (0..a.curve.len())
        .map(|f| if a.curve[f].is_nan() { f32::NAN } else { target.and_then(|t| t.at(f)).unwrap_or(a.curve[f]) })
        .collect();
    let r = a.rate as f64;
    notes
        .iter()
        .map(|n| {
            // frame indices truncated, as the renderer does
            let i1 = ((n.end * r) as usize).min(base.len());
            let i0 = ((n.start * r) as usize).min(i1);
            median(base[i0..i1].to_vec())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::mfcc;
    use crate::pitch::analyze;
    use crate::singer::{sing, song, Voice};

    const SR: f32 = 24000.0;

    #[test]
    fn note_names_and_tuning_wrap() {
        assert_eq!(note_name(60.0), "C4");
        assert_eq!(note_name(69.4), "A4");
        assert_eq!(note_name(70.6), "B4");
        let mk = |c: f32| NoteStats {
            i: 0,
            start: 0.0,
            end: 1.0,
            kind: NoteKind::Stable,
            sung: Some(60.0 + c / 100.0),
            nearest: None,
            cents: Some(c),
            out: None,
            shift: 0.0,
            drift: 0.0,
            formant: 0.0,
            gain: 0.0,
            speed: None,
            confidence: 1.0,
            voiced: 1.0,
            level_db: 0.0,
            slope_cents: 0.0,
            spread_cents: 0.0,
            vibrato: None,
            step_cents: 0.0,
        };
        // a take 48 cents sharp throughout reads as +48, not as a mix of +48 and −52
        let t = tuning(&[mk(48.0), mk(-49.0), mk(46.0), mk(50.0), mk(-47.0)]);
        let o = t.offset_cents.unwrap();
        assert!((o - 48.0).abs() < 4.0 || (o + 52.0).abs() < 4.0, "{o}");
    }

    #[test]
    fn a_take_in_another_key_is_compared_by_its_interval() {
        // the same melody, the reference a fifth lower in another voice and tempo
        let s = song(17, 18);
        let take = sing(&s, &Voice::default(), &[], 0.0, SR);
        let rv = Voice { formant: 1.1, tempo: 0.9, jitter: 0.1, transpose: -7.0, vib_rate: 5.5, vib_depth: 0.2, seed: 5 };
        let reference = sing(&s, &rv, &[], 0.0, SR);
        let (ta, ra) = (analyze(&take.audio, SR), analyze(&reference.audio, SR));
        let (tc, rc) = (mfcc(&take.audio, SR), mfcc(&reference.audio, SR));
        let spans: Vec<(f64, f64)> = ta.notes.iter().map(|n| (n.start, n.end)).collect();
        let c = compare(&ta, &tc, (0.0, take.audio.len() as f64 / SR as f64), &spans, &ra, &rc);
        assert!(c.related, "score {}", c.score);
        assert_eq!(c.intervals[0].semitones, 7, "{:?}", c.intervals);
        let close = c.notes.iter().filter(|m| m.diff_cents.is_some_and(|d| d.abs() < 50.0)).count();
        assert!(close as f32 >= 0.7 * c.notes.len() as f32, "{close} of {} notes match", c.notes.len());
        assert!(c.same_notes.unwrap() > 0.8, "{:?}", c.same_notes);
    }

    #[test]
    fn suggestions_fix_slips_and_keep_intended_notes() {
        let s = song(29, 16);
        // notes 0..: small slips, plus note 5 sung a whole tone off (an intended variation)
        let mut detune = vec![0f32; s.len()];
        detune[1] = 40.0;
        detune[3] = -35.0;
        detune[5] = 200.0;
        let take = sing(&s, &Voice::default(), &detune, 0.0, SR);
        let reference = sing(
            &s,
            &Voice { formant: 1.15, tempo: 0.95, jitter: 0.1, transpose: 12.0, vib_rate: 6.0, vib_depth: 0.2, seed: 3 },
            &[],
            0.0,
            SR,
        );
        let (ta, ra) = (analyze(&take.audio, SR), analyze(&reference.audio, SR));
        let (tc, rc) = (mfcc(&take.audio, SR), mfcc(&reference.audio, SR));
        let (notes, _) = clip_notes(None, &ta);
        let spans: Vec<(f64, f64)> = notes.iter().map(|n| (n.start, n.end)).collect();
        let c = compare(&ta, &tc, (0.0, take.audio.len() as f64 / SR as f64), &spans, &ra, &rc);
        assert_eq!(c.intervals[0].semitones, -12);
        let st = note_stats(&ta, &notes, &ta.curve);
        let sug = suggest(&st, &base_centers(&ta, &notes, None), Some(&c), &SuggestParams::default());
        // which detected note holds each sung note's middle
        let at = |k: usize| {
            let (a, b, _) = take.notes[k];
            let mid = (a + b) / 2.0;
            notes.iter().position(|n| n.start <= mid && mid < n.end).unwrap()
        };
        let d = |k: usize| &sug[at(k)];
        assert_eq!(d(1).decision, Decision::Edit, "{:?}", d(1));
        assert!((d(1).change_cents.unwrap() + 40.0).abs() < 15.0, "{:?}", d(1));
        assert_eq!(d(3).decision, Decision::Edit, "{:?}", d(3));
        assert!(d(3).change_cents.unwrap() > 15.0, "{:?}", d(3));
        assert_eq!(d(5).decision, Decision::Keep, "{:?}", d(5));
        assert!(d(5).reason.contains("different note"), "{:?}", d(5));
        // a detected note holding two sung notes of the same pitch (legato) is judged as one:
        // only notes alone in their detected note count as in tune
        let alone = |k: usize| (0..s.len()).all(|j| j == k || at(j) != at(k));
        let touched_in_tune = (0..s.len())
            .filter(|&k| detune[k] == 0.0 && alone(k) && d(k).decision == Decision::Edit && d(k).change_cents.unwrap().abs() > 20.0)
            .count();
        assert_eq!(touched_in_tune, 0, "in-tune notes moved");
    }
}
