//! Tune to Reference: derive a target pitch for a sung take from another performance
//! of the same melody (a reference vocal), without generating new audio.
//!
//! 1. **Align** the take to the reference, coarse to fine: subsequence DTW on content
//!    (MFCC) plus voicing and pitch class at 25 frames/s finds where in the reference
//!    the take is; a banded DTW at 100 frames/s around that path then adds local
//!    pitch *shape* (slope and contour relative to the local mean, so a take that is
//!    off-key still matches its own melody). The take's time axis is never changed:
//!    the alignment only maps reference time onto it.
//! 2. **Confidence** per frame from how much better the matched cost is than chance,
//!    how well the pitch contours correlate, and how far apart the pitches are. Where
//!    it is low nothing is corrected; a reference that is not this song is rejected.
//! 3. **Register**: the reference moves by the octave that brings it closest to the take.
//! 4. **Target**: note centres come from the reference's notes (or the nearest scale note,
//!    `influence`), transitions are smoothed, and the singer's own vibrato and
//!    transitions are kept (`vibrato`) instead of copying the reference frame by frame.

use crate::content::{ContentFeatures, N_MFCC};
use crate::model::RefTune;
use crate::pitch::{scale_mask, segment, snap, F0Estimator, NoteBlob, PitchAnalysis};
use crate::sources::{mono_of, Audio};

pub struct Performance<'a> {
    pub pitch: &'a PitchAnalysis,
    pub content: &'a ContentFeatures,
}

/// Below this overall score the reference is taken to be a different song or take.
pub const MIN_SCORE: f32 = 0.3;
/// Coarse-alignment cell budget; longer material is aligned at a lower coarse rate.
const MAX_CELLS: usize = 40_000_000;
const SHAPE: f32 = 0.6;
/// Cepstral coefficients (c1…) compared as content.
const ENVELOPE: usize = 6;
/// A re-aligned (bridged) stretch must match content at most this far toward chance.
const BRIDGE_RATIO: f32 = 0.45;
/// Above this median content ratio (1 = chance) a reference is refused whatever else matches.
const MAX_CONTENT_RATIO: f32 = 0.6;

#[derive(Clone, Debug)]
pub struct Alignment {
    /// First analysis frame (pitch rate) of the aligned region of the take.
    pub start: usize,
    /// Per analysis frame of the region: seconds into the reference (NaN = none).
    pub ref_time: Vec<f32>,
    /// Reference pitch (MIDI, untransposed) at `ref_time`; NaN = unvoiced.
    pub reference_f0: Vec<f32>,
    /// 0..1 per frame; 0 = do not correct.
    pub confidence: Vec<f32>,
    /// Automatic transposition of the reference, semitones (a multiple of 12).
    pub octave: i32,
    /// Median confidence over the take's voiced frames.
    pub score: f32,
    pub reason: Option<String>,
}

#[derive(Clone, Copy)]
struct Fr {
    m: [f32; N_MFCC],
    /// audible
    a: bool,
    /// voiced
    v: bool,
    /// pitch, MIDI (NaN unvoiced)
    p: f32,
    /// pitch slope, semitones per frame (NaN unknown)
    d: f32,
    /// pitch relative to the local mean (NaN unknown)
    rel: f32,
}

fn frames_of(perf: &Performance, f0: usize, f1: usize) -> Vec<Fr> {
    let (c, pa) = (perf.content, perf.pitch);
    let top = c.energy.iter().copied().fold(f32::MIN, f32::max);
    let p: Vec<f32> = (f0..f1)
        .map(|k| {
            let i = (k as f32 / c.rate * pa.rate).round() as usize;
            pa.curve.get(i).copied().unwrap_or(f32::NAN)
        })
        .collect();
    let n = f1 - f0;
    (0..n)
        .map(|i| {
            let mut m = [0f32; N_MFCC];
            m.copy_from_slice(c.frame(f0 + i));
            let d = if i >= 2 && i + 2 < n && p[i - 2..=i + 2].iter().all(|x| !x.is_nan()) {
                ((p[i + 2] - p[i - 2]) / 4.0).clamp(-1.0, 1.0)
            } else {
                f32::NAN
            };
            // relative to the local median: one slipped note must not shift its neighbours
            let rel = if p[i].is_nan() {
                f32::NAN
            } else {
                let w: Vec<f32> = p[i.saturating_sub(15)..(i + 16).min(n)]
                    .iter()
                    .copied()
                    .filter(|x| !x.is_nan())
                    .collect();
                if w.len() >= 5 {
                    p[i] - median(w).unwrap_or(p[i])
                } else {
                    f32::NAN
                }
            };
            Fr {
                m,
                a: c.energy[f0 + i] > top - 40.0,
                v: !p[i].is_nan(),
                p: p[i],
                d,
                rel,
            }
        })
        .collect()
}

fn coarse(fr: &[Fr], q: usize) -> Vec<Fr> {
    fr.chunks(q)
        .map(|ch| {
            let mut m = [0f32; N_MFCC];
            for f in ch {
                for (a, b) in m.iter_mut().zip(&f.m) {
                    *a += b / ch.len() as f32;
                }
            }
            let mut ps: Vec<f32> = ch.iter().filter(|f| f.v).map(|f| f.p).collect();
            ps.sort_by(f32::total_cmp);
            let v = ps.len() * 2 >= ch.len();
            let a = ch.iter().filter(|f| f.a).count() * 2 >= ch.len();
            Fr {
                m,
                a,
                v,
                p: if v { ps[ps.len() / 2] } else { f32::NAN },
                d: f32::NAN,
                rel: f32::NAN,
            }
        })
        .collect()
}

/// Cosine distance (0..1) of the spectral envelopes: only the lowest cepstral
/// coefficients, because the higher ones follow the harmonics, and the same vowel an
/// octave up (or in a smaller voice) samples its envelope at entirely other points.
fn timbre(a: &Fr, b: &Fr) -> f32 {
    let (mut d, mut na, mut nb) = (0f32, 0f32, 0f32);
    for i in 0..ENVELOPE {
        d += a.m[i] * b.m[i];
        na += a.m[i] * a.m[i];
        nb += b.m[i] * b.m[i];
    }
    0.5 * (1.0 - d / (na * nb).sqrt().max(1e-6))
}

fn cost(a: &Fr, b: &Fr, pitch_class: f32, shape: f32) -> f32 {
    let content = match (a.a, b.a) {
        (true, true) => timbre(a, b),
        (false, false) => 0.05,
        _ => 0.6,
    };
    let mut c = content;
    if a.v != b.v {
        c += 0.35;
    } else if a.v {
        let dd = (a.p - b.p).rem_euclid(12.0);
        c += pitch_class * ((dd.min(12.0 - dd) - 0.5) / 2.5).clamp(0.0, 1.0);
        if shape > 0.0 {
            if !a.d.is_nan() && !b.d.is_nan() {
                c += shape * 0.5 * ((a.d - b.d).abs() / 0.15).min(1.0);
            }
            if !a.rel.is_nan() && !b.rel.is_nan() {
                c += shape * 0.5 * ((a.rel - b.rel).abs() / 1.5).min(1.0);
            }
        }
    }
    c
}

/// What is being sung, and whether: MFCC distance and voicing, no pitch.
fn content_cost(a: &Fr, b: &Fr) -> f32 {
    let c = match (a.a, b.a) {
        (true, true) => timbre(a, b),
        (false, false) => 0.05,
        _ => 0.6,
    };
    c + if a.v != b.v { 0.35 } else { 0.0 }
}

/// Subsequence DTW: all of `0..n` is matched to some stretch of `0..m`.
/// Returns, for each `i`, the mean matched `j`.
fn dtw_subsequence(n: usize, m: usize, c: impl Fn(usize, usize) -> f32, pen: f32) -> Vec<f32> {
    let mut bp = vec![0u8; n * m];
    let mut prev: Vec<f32> = (0..m).map(|j| c(0, j)).collect();
    let mut cur = vec![0f32; m];
    bp[..m].fill(3);
    for i in 1..n {
        for j in 0..m {
            let (mut best, mut b) = (prev[j] + pen, 1u8);
            if j > 0 {
                if prev[j - 1] < best {
                    (best, b) = (prev[j - 1], 0);
                }
                if cur[j - 1] + pen < best {
                    (best, b) = (cur[j - 1] + pen, 2);
                }
            }
            cur[j] = c(i, j) + best;
            bp[i * m + j] = b;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let mut j = (0..m)
        .min_by(|&a, &b| prev[a].total_cmp(&prev[b]))
        .unwrap_or(0);
    let mut i = n - 1;
    backtrack(n, |i, j| bp[i * m + j], &mut i, &mut j)
}

fn backtrack(n: usize, bp: impl Fn(usize, usize) -> u8, i: &mut usize, j: &mut usize) -> Vec<f32> {
    let (mut sum, mut cnt) = (vec![0f64; n], vec![0u32; n]);
    loop {
        sum[*i] += *j as f64;
        cnt[*i] += 1;
        match bp(*i, *j) {
            0 => {
                *i -= 1;
                *j -= 1;
            }
            1 => *i -= 1,
            2 => *j -= 1,
            _ => break,
        }
    }
    sum.iter()
        .zip(&cnt)
        .map(|(s, &k)| {
            if k > 0 {
                (*s / k as f64) as f32
            } else {
                f32::NAN
            }
        })
        .collect()
}

/// Banded DTW: row `u` may only match columns within `±w` of `center[u]`.
fn dtw_band(
    m: usize,
    center: &[f32],
    w: usize,
    c: impl Fn(usize, usize) -> f32,
    pen: f32,
) -> Vec<f32> {
    let n = center.len();
    let mut lo = vec![0usize; n];
    let mut hi = vec![0usize; n];
    for u in 0..n {
        let k = center[u].round() as i64;
        lo[u] = (k - w as i64).clamp(0, m as i64 - 1) as usize;
        hi[u] = (k + w as i64).clamp(0, m as i64 - 1) as usize;
        if u > 0 {
            lo[u] = lo[u].max(lo[u - 1]).min(hi[u - 1] + 1).min(m - 1);
            hi[u] = hi[u].max(hi[u - 1]).max(lo[u]);
        }
    }
    let mut off = vec![0usize; n + 1];
    for u in 0..n {
        off[u + 1] = off[u] + hi[u] - lo[u] + 1;
    }
    let mut dd = vec![f32::INFINITY; off[n]];
    let mut bp = vec![3u8; off[n]];
    let at = |u: usize, j: usize| -> Option<usize> {
        (j >= lo[u] && j <= hi[u]).then(|| off[u] + j - lo[u])
    };
    for j in lo[0]..=hi[0] {
        dd[off[0] + j - lo[0]] = c(0, j);
    }
    for u in 1..n {
        for j in lo[u]..=hi[u] {
            let (mut best, mut b) = (f32::INFINITY, 3u8);
            if j > 0 {
                if let Some(x) = at(u - 1, j - 1) {
                    (best, b) = (dd[x], 0);
                }
            }
            if let Some(x) = at(u - 1, j) {
                if dd[x] + pen < best {
                    (best, b) = (dd[x] + pen, 1);
                }
            }
            if j > lo[u] {
                let x = off[u] + j - 1 - lo[u];
                if dd[x] + pen < best {
                    (best, b) = (dd[x] + pen, 2);
                }
            }
            let k = off[u] + j - lo[u];
            dd[k] = c(u, j) + best;
            bp[k] = b;
        }
    }
    let mut j = (lo[n - 1]..=hi[n - 1])
        .min_by(|&a, &b| dd[off[n - 1] + a - lo[n - 1]].total_cmp(&dd[off[n - 1] + b - lo[n - 1]]))
        .unwrap_or(lo[n - 1]);
    let mut u = n - 1;
    backtrack(n, |u, j| bp[off[u] + j - lo[u]], &mut u, &mut j)
}

/// Mean of the finite values within `±h`; NaN stays NaN.
pub(crate) fn smooth(x: &[f32], h: usize) -> Vec<f32> {
    let n = x.len();
    let mut cs = vec![(0f64, 0u32); n + 1];
    for i in 0..n {
        let (s, k) = cs[i];
        cs[i + 1] = if x[i].is_nan() {
            (s, k)
        } else {
            (s + x[i] as f64, k + 1)
        };
    }
    (0..n)
        .map(|i| {
            if x[i].is_nan() {
                return f32::NAN;
            }
            let (a, b) = (i.saturating_sub(h), (i + h + 1).min(n));
            let (s, k) = (cs[b].0 - cs[a].0, cs[b].1 - cs[a].1);
            (s / k.max(1) as f64) as f32
        })
        .collect()
}

fn min_filter(x: &[f32], h: usize) -> Vec<f32> {
    (0..x.len())
        .map(|i| {
            x[i.saturating_sub(h)..(i + h + 1).min(x.len())]
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min)
        })
        .collect()
}

fn median(mut v: Vec<f32>) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

/// Linear interpolation of `x` at fractional index `t`; NaN neighbours are skipped.
pub(crate) fn lerp_at(x: &[f32], t: f32) -> f32 {
    if x.is_empty() || !t.is_finite() || t < -0.5 || t > x.len() as f32 - 0.5 {
        return f32::NAN;
    }
    let t = t.clamp(0.0, (x.len() - 1) as f32);
    let i = t.floor() as usize;
    let j = (i + 1).min(x.len() - 1);
    let w = t - i as f32;
    match (x[i].is_nan(), x[j].is_nan()) {
        (false, false) => x[i] + (x[j] - x[i]) * w,
        (false, true) if w < 0.5 => x[i],
        (true, false) if w >= 0.5 => x[j],
        _ => f32::NAN,
    }
}

/// A DTW path (reference frame per take frame) smoothed and made monotone.
fn monotone(path: &[f32]) -> Vec<f32> {
    let mut map = smooth(path, 4);
    for u in 1..map.len() {
        map[u] = map[u].max(map[u - 1]);
    }
    map
}

/// Per take frame: how far to believe a path, and the reference pitch it lands on.
struct Evidence {
    /// 0..1 before the register is known.
    conf0: Vec<f32>,
    /// Reference pitch at the mapped frame (MIDI; NaN = unvoiced).
    rp: Vec<f32>,
    /// `conf0` judging the tempo map alone, whatever the content says: repeated syllables
    /// match well on a wrong path too, so paths are chosen by this.
    strict: Vec<f32>,
    /// 0..1; 0 where the tempo map is implausible.
    strict_ok: Vec<f32>,
    /// Content cost along the path relative to chance (smoothed; 1 = no better).
    ratio: Vec<f32>,
}

/// Judges the path `map` of take frames `uf` into reference frames `rf`; `chance` is the
/// content cost against arbitrary reference frames.
fn evidence(uf: &[Fr], rf: &[Fr], map: &[f32], chance: &[f32]) -> Evidence {
    let (n, m) = (uf.len(), rf.len());
    let jr = |u: usize| (map[u].round().max(0.0) as usize).min(m - 1);
    // matched content (pitch left out, so an off-key take still matches) against chance
    let pc: Vec<f32> = (0..n).map(|u| content_cost(&uf[u], &rf[jr(u)])).collect();
    let pcs = smooth(&pc, 25);
    let ratio: Vec<f32> = (0..n).map(|u| pcs[u] / chance[u].max(1e-3)).collect();
    // ... a plausible tempo map: locally close to a straight line with a sane slope
    let warp: Vec<f32> = (0..n)
        .map(|u| {
            let (a, b) = (u.saturating_sub(100), (u + 101).min(n));
            let k = (b - a) as f32;
            let (mx, my) = ((a + b - 1) as f32 / 2.0, map[a..b].iter().sum::<f32>() / k);
            let (mut sxy, mut sxx) = (0f32, 0f32);
            for (i, &y) in map[a..b].iter().enumerate() {
                let x = (a + i) as f32 - mx;
                sxy += x * (y - my);
                sxx += x * x;
            }
            let slope = sxy / sxx.max(1.0);
            let res = median(
                map[a..b]
                    .iter()
                    .enumerate()
                    .map(|(i, &y)| (y - my - slope * ((a + i) as f32 - mx)).abs())
                    .collect(),
            )
            .unwrap_or(0.0)
                * 1.25;
            if !(0.6..=1.67).contains(&slope) {
                return f32::INFINITY;
            }
            res
        })
        .collect();
    // Content alone may not veto: across octaves and voices some vowels look unlike
    // themselves (a first formant below the fundamental). The tempo map decides.
    let content_n: Vec<f32> = (0..n)
        .map(|u| ((1.0 - ratio[u]) / 0.4).clamp(0.0, 1.0))
        .collect();
    // ... unless the content matches far better than chance: a reference sung with its own
    // rubato is irregular against the take, and that is no sign of a wrong path
    let strict_ok: Vec<f32> = (0..n)
        .map(|u| ((6.0 - warp[u]) / 3.0).clamp(0.0, 1.0))
        .collect();
    let warp_ok: Vec<f32> = (0..n)
        .map(|u| strict_ok[u].max(((0.2 - ratio[u]) / 0.1).clamp(0.0, 1.0)))
        .collect();
    // ... and how well the pitch contours move together
    let rp: Vec<f32> = (0..n).map(|u| rf[jr(u)].p).collect();
    let conf_shape: Vec<f32> = (0..n)
        .map(|u| {
            let (a, b) = (u.saturating_sub(40), (u + 41).min(n));
            let mut pairs: Vec<(f32, f32)> = (a..b)
                .filter(|&k| uf[k].v && !rp[k].is_nan())
                .map(|k| (uf[k].p, rp[k]))
                .collect();
            // octave slips of either detector are outliers, not evidence against the match
            if let Some(md) = median(pairs.iter().map(|p| p.0 - p.1).collect()) {
                pairs.retain(|p| (p.0 - p.1 - md).abs() < 4.0);
            }
            if pairs.len() < 20 {
                return 0.5;
            }
            let k = pairs.len() as f32;
            let (mx, my) = (
                pairs.iter().map(|p| p.0).sum::<f32>() / k,
                pairs.iter().map(|p| p.1).sum::<f32>() / k,
            );
            let (mut sxy, mut sxx, mut syy) = (0f32, 0f32, 0f32);
            for &(x, y) in &pairs {
                sxy += (x - mx) * (y - my);
                sxx += (x - mx).powi(2);
                syy += (y - my).powi(2);
            }
            let (sx, sy) = ((sxx / k).sqrt(), (syy / k).sqrt());
            if sx < 0.25 || sy < 0.25 {
                return 0.6; // a held note: the contour says little
            }
            ((sxy / (sxx * syy).sqrt() - 0.1) / 0.5).clamp(0.0, 1.0)
        })
        .collect();
    // an irregular tempo map vetoes; contour and content add up as evidence
    let support: Vec<f32> = (0..n)
        .map(|u| (0.35 + 0.35 * content_n[u] + 0.5 * conf_shape[u]).min(1.0))
        .collect();
    let conf0 = (0..n).map(|u| warp_ok[u] * support[u]).collect();
    let strict = (0..n).map(|u| strict_ok[u] * support[u]).collect();
    Evidence {
        conf0,
        rp,
        strict,
        strict_ok,
        ratio,
    }
}

/// `map` with each implausible stretch (`ok` < 0.5) replaced by a straight line between
/// the plausible frames around it (continued at the slope of its neighbour at either end);
/// `None` if nothing or everything is implausible.
fn bridged(map: &[f32], ok: &[f32]) -> Option<Vec<f32>> {
    let n = map.len();
    let good: Vec<bool> = ok.iter().map(|&w| w >= 0.5).collect();
    if good.iter().all(|&g| g) || !good.iter().any(|&g| g) {
        return None;
    }
    let slope = |a: usize, b: usize| (map[b] - map[a]) / (b - a).max(1) as f32;
    let mut out = map.to_vec();
    let mut u = 0;
    while u < n {
        if good[u] {
            u += 1;
            continue;
        }
        let a = u;
        while u < n && !good[u] {
            u += 1;
        }
        let (l, r) = (a.checked_sub(1), (u < n).then_some(u));
        for (k, y) in out.iter_mut().enumerate().take(u).skip(a) {
            *y = match (l, r) {
                (Some(l), Some(r)) => map[l] + slope(l, r) * (k - l) as f32,
                (None, Some(r)) => map[r] - slope(r, (r + 100).min(n - 1)) * (r - k) as f32,
                (Some(l), None) => map[l] + slope(l.saturating_sub(100), l) * (k - l) as f32,
                (None, None) => map[k],
            }
            .max(0.0);
        }
    }
    Some(out)
}

/// Aligns `region` (seconds of the take's source) to anywhere in `reference`.
///
/// Pitch evidence (pitch class, contour shape) sharpens the path when the take is
/// roughly in tune, but a take several semitones off its melody, against a reference
/// in another voice, can be pulled to wrong places by it; then a path from content
/// alone is tried as well and the better-scoring one wins.
pub fn align(take: &Performance, region: (f64, f64), reference: &Performance) -> Alignment {
    let with_pitch = align_pass(take, region, reference, 1.0);
    if with_pitch.score >= 0.8 {
        return with_pitch;
    }
    let content_only = align_pass(take, region, reference, 0.0);
    if content_only.score > with_pitch.score {
        content_only
    } else {
        with_pitch
    }
}

/// One alignment; `pitch_weight` scales the pitch terms of the path cost (0 = content only).
fn align_pass(
    take: &Performance,
    region: (f64, f64),
    reference: &Performance,
    pitch_weight: f32,
) -> Alignment {
    let rate = take.pitch.rate;
    let a0 = ((region.0.max(0.0) * rate as f64).round() as usize).min(take.pitch.curve.len());
    let a1 = ((region.1 * rate as f64).round() as usize)
        .min(take.pitch.curve.len())
        .max(a0);
    let empty = |reason: &str| Alignment {
        start: a0,
        ref_time: vec![f32::NAN; a1 - a0],
        reference_f0: vec![f32::NAN; a1 - a0],
        confidence: vec![0.0; a1 - a0],
        octave: 0,
        score: 0.0,
        reason: Some(reason.to_string()),
    };
    let cr = take.content.rate;
    let u0 = ((a0 as f32 / rate * cr).floor() as usize).min(take.content.frames());
    let u1 = ((a1 as f32 / rate * cr).ceil() as usize).min(take.content.frames());
    let (n, m) = (u1.saturating_sub(u0), reference.content.frames());
    if n < 20 || m < 20 {
        return empty("too short to align");
    }
    let uf = frames_of(take, u0, u1);
    let rf = frames_of(reference, 0, m);
    if uf.iter().filter(|f| f.v).count() < 10 {
        return empty("nothing sung here");
    }
    if rf.iter().filter(|f| f.v).count() < 10 {
        return empty("the reference has no sung pitch");
    }
    // coarse: where in the reference is this take?
    let mut q = 4usize;
    while (n / q + 1) * (m / q + 1) > MAX_CELLS {
        q += 1;
    }
    let (uc, rc) = (coarse(&uf, q), coarse(&rf, q));
    let path_c = dtw_subsequence(
        uc.len(),
        rc.len(),
        |i, j| cost(&uc[i], &rc[j], 0.2 * pitch_weight, 0.0),
        0.15,
    );
    let center: Vec<f32> = (0..n)
        .map(|u| {
            let x = (u as f32 + 0.5) / q as f32 - 0.5;
            (lerp_at(&path_c, x) + 0.5) * q as f32 - 0.5
        })
        .collect();
    // fine: local alignment with pitch shape
    let jf = dtw_band(
        m,
        &center,
        q * 3 + 4,
        |u, j| cost(&uf[u], &rf[j], 0.2 * pitch_weight, SHAPE * pitch_weight),
        0.1,
    );
    let mut map = monotone(&jf);
    // chance level of the content match per take frame (independent of the path)
    let probes: Vec<usize> = (0..24).map(|k| (k * m + m / 2) / 24).collect();
    let chance: Vec<f32> = (0..n)
        .map(|u| {
            median(
                probes
                    .iter()
                    .map(|&j| content_cost(&uf[u], &rf[j]))
                    .collect(),
            )
            .unwrap_or(1.0)
        })
        .collect();
    let chance = smooth(&chance, 25);
    let mut ev = evidence(&uf, &rf, &map, &chance);
    // a jump-and-stall of the path (repeated syllables, a misleading contour) shows as an
    // implausible tempo map: bridge it with a straight line and align again, narrowly, around
    // it. A straight path is plausible by construction, so over the bridged stretch the
    // content has to vouch for it (an unrelated reference stays near chance there).
    if let Some(line) = bridged(&map, &ev.strict_ok) {
        let jf2 = dtw_band(
            m,
            &line,
            (0.15 * cr) as usize,
            |u, j| cost(&uf[u], &rf[j], 0.2 * pitch_weight, SHAPE * pitch_weight),
            0.1,
        );
        let map2 = monotone(&jf2);
        let ev2 = evidence(&uf, &rf, &map2, &chance);
        let bridged_ratio = median(
            (0..n)
                .filter(|&u| ev.strict_ok[u] < 0.5 && uf[u].v)
                .map(|u| ev2.ratio[u])
                .collect(),
        )
        .unwrap_or(1.0);
        let total = |e: &Evidence| {
            (0..n)
                .filter(|&u| uf[u].v)
                .map(|u| e.strict[u])
                .sum::<f32>()
        };
        if bridged_ratio < BRIDGE_RATIO && total(&ev2) > total(&ev) {
            (map, ev) = (map2, ev2);
        }
    }
    let Evidence {
        conf0, rp, ratio, ..
    } = ev;
    // register: the octave that brings the reference closest to the take
    let diffs = |min_conf: f32| -> Vec<f32> {
        (0..n)
            .filter(|&u| uf[u].v && !rp[u].is_nan() && conf0[u] >= min_conf)
            .map(|u| uf[u].p - rp[u])
            .collect()
    };
    let mut ds = diffs(0.4);
    if ds.len() < 20 {
        ds = diffs(0.0);
    }
    let octave = (-3..=3).min_by(|&a, &b| {
        let e = |k: i32| {
            median(ds.iter().map(|d| (d - 12.0 * k as f32).abs()).collect()).unwrap_or(f32::MAX)
        };
        e(a).total_cmp(&e(b))
    });
    let octave = octave.unwrap_or(0) * 12;
    // a take this far from the reference is not singing that note (or a detector slipped):
    // a per-frame judgement, so it is smoothed far less than the trust in the alignment
    let far: Vec<f32> = (0..n)
        .map(|u| {
            if !uf[u].v || rp[u].is_nan() {
                return 1.0;
            }
            let d = uf[u].p - rp[u] - octave as f32;
            // exactly an octave or two off is the detector slipping, which the target folds back
            if d.abs() > 6.0 && (d - 12.0 * (d / 12.0).round()).abs() < 1.0 {
                return 1.0;
            }
            ((7.0 - d.abs()) / 2.0).clamp(0.0, 1.0)
        })
        .collect();
    let trust = smooth(&min_filter(&conf0, 5), 10);
    let far = smooth(&min_filter(&far, 2), 2);
    let conf: Vec<f32> = (0..n).map(|u| trust[u] * far[u]).collect();
    // overall, the content must match clearly better than chance: a melody shape alone (an
    // arpeggio against a song that outlines the same chord) is no evidence of the same part
    let content = median((0..n).filter(|&u| uf[u].v).map(|u| ratio[u]).collect()).unwrap_or(1.0);
    let score = median((0..n).filter(|&u| uf[u].v).map(|u| conf[u]).collect()).unwrap_or(0.0)
        * ((MAX_CONTENT_RATIO + 0.05 - content) / 0.1).clamp(0.0, 1.0);
    // back to the analysis rate, over the region
    let rr = reference.pitch.rate;
    let mut out = Alignment {
        start: a0,
        ref_time: vec![],
        reference_f0: vec![],
        confidence: vec![],
        octave,
        score,
        reason: None,
    };
    for f in a0..a1 {
        let x = f as f32 / rate * cr - u0 as f32;
        let j = lerp_at(&map, x);
        let t = j / reference.content.rate;
        out.ref_time.push(t);
        out.reference_f0
            .push(lerp_at(&reference.pitch.curve, t * rr));
        out.confidence.push(lerp_at(&conf, x).clamp(0.0, 1.0));
    }
    if score < MIN_SCORE {
        out.confidence.fill(0.0);
        out.reason = Some("the reference does not match this take".into());
    }
    out
}

/// Tune to Reference result over the aligned region (frames at `rate`, from `start`).
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PitchTarget {
    pub rate: f32,
    /// First analysis frame of the region in the take's source.
    pub start: usize,
    /// The take's pitch (MIDI; NaN = unvoiced).
    pub source_f0: Vec<f32>,
    /// The reference's pitch mapped onto the take's time axis and transposed by `octave`.
    pub reference_f0: Vec<f32>,
    /// The pitch to render (NaN = unvoiced, left untouched).
    pub target_f0: Vec<f32>,
    /// 0..1; where 0, `target_f0` is the take's own pitch.
    pub alignment_confidence: Vec<f32>,
    /// Seconds into the reference for each frame.
    pub ref_time: Vec<f32>,
    /// Transposition applied to the reference (semitones).
    pub octave: i32,
    /// The reference's notes on the take's time axis (seconds of the take's source).
    pub notes: Vec<NoteBlob>,
    pub score: f32,
    pub reason: Option<String>,
}

impl PitchTarget {
    /// Target pitch at analysis frame `f` of the source, if the region covers it.
    pub fn at(&self, f: usize) -> Option<f32> {
        f.checked_sub(self.start)
            .and_then(|i| self.target_f0.get(i))
            .copied()
            .filter(|v| !v.is_nan())
    }
}

/// Correction weight for an alignment confidence: none below 0.2, full from 0.45
/// (half-trusted alignments would half-correct, which sounds like neither).
pub fn gate(conf: f32) -> f32 {
    let x = ((conf - 0.2) / 0.25).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The target pitch for `take` given an alignment and the user's parameters.
/// `spans` are the take's notes (seconds; the editor's segmentation).
pub fn target(
    take: &PitchAnalysis,
    spans: &[(f64, f64)],
    al: &Alignment,
    p: &RefTune,
) -> PitchTarget {
    let n = al.ref_time.len();
    let s = al.start;
    let rate = take.rate;
    let u: Vec<f32> = (s..s + n)
        .map(|f| take.curve.get(f).copied().unwrap_or(f32::NAN))
        .collect();
    let octave = p.octave.unwrap_or(al.octave);
    let r: Vec<f32> = al.reference_f0.iter().map(|x| x + octave as f32).collect();
    // the reference's notes, where the take is sung
    let rmask: Vec<f32> = (0..n)
        .map(|f| if u[f].is_nan() { f32::NAN } else { r[f] })
        .collect();
    let levels: Vec<f32> = (s..s + n)
        .map(|f| take.levels.get(f).copied().unwrap_or(0.0))
        .collect();
    let rnotes = segment(&rmask, &levels);
    let mut refc = vec![f32::NAN; n];
    let mut cuts = vec![false; n + 1];
    for nt in &rnotes {
        let (fa, fb) = (
            ((nt.start * rate as f64).round() as usize).min(n),
            ((nt.end * rate as f64).round() as usize).min(n),
        );
        refc[fa..fb].fill(nt.pitch);
        cuts[fa] = true;
        cuts[fb] = true;
    }
    // the singer's note centres; what moves around them is drift (slow) and vibrato/transitions
    // (fast). A take span covering two reference notes (a wrong note sung as its neighbour) is
    // two notes: one centre over the step would leak the step into the vibrato.
    let mut uc = vec![f32::NAN; n];
    let mut vib = vec![0f32; n];
    // share of each note's sung frames the reference has a note for
    let mut cover = vec![0f32; n];
    // ... and the reference's median pitch over it, where it sings most of it
    let mut sung_ref = vec![f32::NAN; n];
    for &(a, b) in spans {
        let fa = ((a * rate as f64).round() as usize)
            .saturating_sub(s)
            .min(n);
        let fb = ((b * rate as f64).round() as usize)
            .saturating_sub(s)
            .min(n);
        if fb <= fa {
            continue;
        }
        let sung = u[fa..fb].iter().filter(|x| !x.is_nan()).count();
        let covered = (fa..fb)
            .filter(|&f| !u[f].is_nan() && !refc[f].is_nan())
            .count();
        let rv: Vec<f32> = (fa..fb)
            .filter(|&f| !u[f].is_nan() && !r[f].is_nan())
            .map(|f| r[f])
            .collect();
        let rmed = if rv.len() * 2 >= sung {
            median(rv).unwrap_or(f32::NAN)
        } else {
            f32::NAN
        };
        cover[fa..fb].fill(covered as f32 / sung.max(1) as f32);
        sung_ref[fa..fb].fill(rmed);
        let mut edges: Vec<usize> = (fa + 1..fb).filter(|&f| cuts[f]).collect();
        edges.insert(0, fa);
        edges.push(fb);
        for e in edges.windows(2) {
            let (fa, fb) = (e[0], e[1]);
            if fb <= fa {
                continue;
            }
            let Some(c) = median(u[fa..fb].iter().copied().filter(|x| !x.is_nan()).collect())
            else {
                continue;
            };
            // octave slips of the detector are no movement of the voice; transitions into the
            // neighbouring notes are kept as sung but say nothing about the drift
            let dev: Vec<f32> = u[fa..fb]
                .iter()
                .map(|x| if (x - c).abs() > 6.0 { f32::NAN } else { x - c })
                .collect();
            let near: Vec<f32> = dev
                .iter()
                .map(|&d| if d.abs() > 1.5 { f32::NAN } else { d })
                .collect();
            let drift = smooth(
                &smooth(&near, (0.09 * rate) as usize),
                (0.07 * rate) as usize,
            );
            let drift: Vec<f32> = drift
                .iter()
                .map(|d| if d.is_nan() { 0.0 } else { *d })
                .collect();
            for k in 0..fb - fa {
                uc[fa + k] = c;
                if !dev[k].is_nan() {
                    vib[fa + k] = dev[k] - drift[k];
                }
            }
        }
    }
    // sung frames the reference has no note for take the nearest note within 200 ms, at the
    // edges of notes it mostly covers; elsewhere the reference's own pitch over the note if it
    // sings there (too briefly for a note of its own), else the note is left as sung
    let reach = (0.2 * rate) as usize;
    let filled: Vec<f32> = (0..n)
        .map(|f| {
            if !refc[f].is_nan() || u[f].is_nan() {
                return refc[f];
            }
            if cover[f] < 0.3 {
                return sung_ref[f];
            }
            (1..=reach)
                .find_map(|d| {
                    [f.checked_sub(d), Some(f + d).filter(|&g| g < n)]
                        .into_iter()
                        .flatten()
                        .map(|g| refc[g])
                        .find(|v| !v.is_nan())
                })
                .unwrap_or(f32::NAN)
        })
        .collect();
    let mask = scale_mask(p.scale);
    let inf = p.influence.clamp(0.0, 1.0);
    let tc_raw: Vec<f32> = (0..n)
        .map(|f| {
            if u[f].is_nan() || uc[f].is_nan() || filled[f].is_nan() {
                return f32::NAN;
            }
            inf * filled[f] + (1.0 - inf) * snap(uc[f], p.key, mask)
        })
        .collect();
    let h = (0.03 * rate) as usize;
    let tc = smooth(&smooth(&tc_raw, h), h);
    // smoothed over sung frames only: a consonant gap must not fade the correction around it
    let w_raw: Vec<f32> = (0..n)
        .map(|f| {
            if tc[f].is_nan() {
                f32::NAN
            } else {
                gate(al.confidence[f])
            }
        })
        .collect();
    let w = smooth(&smooth(&w_raw, h), h);
    let (corr, keep) = (p.correction.clamp(0.0, 1.0), p.vibrato.clamp(0.0, 1.0));
    let target_f0: Vec<f32> = (0..n)
        .map(|f| {
            if u[f].is_nan() {
                return f32::NAN;
            }
            if tc[f].is_nan() {
                return u[f];
            }
            // an octave slip of the detector is the same note: move it by the same interval
            let d = tc[f] + keep * vib[f] - u[f];
            u[f] + corr * w[f] * (d - 12.0 * (d / 12.0).round())
        })
        .collect();
    let t0 = s as f64 / rate as f64;
    let notes = rnotes
        .iter()
        .map(|nt| NoteBlob {
            start: nt.start + t0,
            end: nt.end + t0,
            pitch: nt.pitch,
            level: nt.level,
        })
        .collect();
    PitchTarget {
        rate,
        start: s,
        source_f0: u,
        reference_f0: r,
        target_f0,
        alignment_confidence: al.confidence.clone(),
        ref_time: al.ref_time.clone(),
        octave,
        notes,
        score: al.score,
        reason: al.reason.clone(),
    }
}

/// Headless Tune to Reference of a whole take: analysis, alignment, target and render.
/// Returns the corrected audio (same length and rate as `take`), the target and the
/// take's analysis.
pub fn tune_audio(
    take: &Audio,
    reference: &Audio,
    p: &RefTune,
    est: Option<&dyn F0Estimator>,
) -> (Audio, PitchTarget, PitchAnalysis) {
    let (tm, rm) = (mono_of(take), mono_of(reference));
    let ta = crate::pitch::analyze_with(&tm, take.sr as f32, est);
    let ra = crate::pitch::analyze_with(&rm, reference.sr as f32, est);
    let (tc, rc) = (
        crate::content::mfcc(&tm, take.sr as f32),
        crate::content::mfcc(&rm, reference.sr as f32),
    );
    let al = align(
        &Performance {
            pitch: &ta,
            content: &tc,
        },
        (0.0, take.seconds()),
        &Performance {
            pitch: &ra,
            content: &rc,
        },
    );
    let spans: Vec<(f64, f64)> = ta.notes.iter().map(|n| (n.start, n.end)).collect();
    let t = target(&ta, &spans, &al, p);
    let tune = crate::model::ClipTune {
        notes: vec![],
        pencil: vec![],
        reference: Some(*p),
    };
    let out = crate::tune_render::render(take, &ta, &tune, Some(&t));
    (out, t, ta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::mfcc;
    use crate::pitch::analyze;
    use crate::singer::{pitch_errors, quantile, sing, song, Rendition, Voice};
    use crate::sources::Audio;

    const SR: f32 = 24000.0;

    fn params() -> RefTune {
        RefTune {
            source: 0,
            correction: 1.0,
            vibrato: 1.0,
            influence: 1.0,
            octave: None,
            key: 0,
            scale: 0,
        }
    }

    fn run(take: &Rendition, reference: &Rendition, p: &RefTune) -> (PitchAnalysis, PitchTarget) {
        let (ta, t, _) = run_with_ref(take, reference, p);
        (ta, t)
    }

    fn run_with_ref(
        take: &Rendition,
        reference: &Rendition,
        p: &RefTune,
    ) -> (PitchAnalysis, PitchTarget, PitchAnalysis) {
        let (ta, ra) = (analyze(&take.audio, SR), analyze(&reference.audio, SR));
        let (tc, rc) = (mfcc(&take.audio, SR), mfcc(&reference.audio, SR));
        let dur = take.audio.len() as f64 / SR as f64;
        let al = align(
            &Performance {
                pitch: &ta,
                content: &tc,
            },
            (0.0, dur),
            &Performance {
                pitch: &ra,
                content: &rc,
            },
        );
        let spans: Vec<(f64, f64)> = ta.notes.iter().map(|n| (n.start, n.end)).collect();
        let t = target(&ta, &spans, &al, p);
        (ta, t, ra)
    }

    fn reference_voice() -> Voice {
        Voice {
            formant: 1.17,
            tempo: 0.88,
            jitter: 0.12,
            transpose: 12.0,
            vib_rate: 6.2,
            vib_depth: 0.25,
            seed: 9,
        }
    }

    #[test]
    fn detuned_take_is_pulled_onto_the_reference_melody() {
        let s = song(7, 28);
        let detune: Vec<f32> = (0..s.len())
            .map(|k| {
                [
                    25.0, -50.0, 100.0, -200.0, 400.0, -25.0, 50.0, -100.0, 200.0, -400.0,
                ][k % 10]
            })
            .collect();
        let take = sing(&s, &Voice::default(), &detune, 0.0, SR);
        let reference = sing(&s, &reference_voice(), &[], 0.0, SR);
        let (_, t, ra) = run_with_ref(&take, &reference, &params());
        assert_eq!(t.octave, -12, "reference sung an octave up");
        // frames where a detector itself is wrong (octave slips, in the take or the reference)
        // are the detector's error, not the correction's
        let ref_ok = |i: usize| -> bool {
            let g = (t.ref_time[i] * ra.rate).round();
            if !g.is_finite() || g < 0.0 {
                return true;
            }
            let g = g as usize;
            match (ra.curve.get(g), reference.sung.get(g)) {
                (Some(a), Some(b)) if !a.is_nan() && !b.is_nan() => (a - b).abs() < 1.0,
                _ => true,
            }
        };
        let ok: Vec<f32> = t
            .source_f0
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                if (v - take.sung[i + t.start]).abs() < 1.0 && ref_ok(i) {
                    v
                } else {
                    f32::NAN
                }
            })
            .collect();
        let slips = t
            .source_f0
            .iter()
            .zip(&ok)
            .filter(|(a, b)| !a.is_nan() && b.is_nan())
            .count();
        let voiced = t.source_f0.iter().filter(|a| !a.is_nan()).count();
        eprintln!("YIN slipped on {slips} of {voiced} voiced frames (take or reference)");
        assert!(
            slips * 100 < voiced * 20,
            "detector slipped on {slips} of {voiced} frames"
        );
        let only_ok = |c: &[f32]| -> Vec<f32> {
            c.iter()
                .zip(&ok)
                .map(|(&x, o)| if o.is_nan() { f32::NAN } else { x })
                .collect()
        };
        let raw = pitch_errors(&take.truth, &take.edge, &only_ok(&t.source_f0), t.start);
        let fixed = pitch_errors(&take.truth, &take.edge, &only_ok(&t.target_f0), t.start);
        let (mr, mf, p90) = (
            raw.iter().sum::<f32>() / raw.len() as f32,
            quantile(&fixed, 0.5),
            quantile(&fixed, 0.9),
        );
        let mae = fixed.iter().sum::<f32>() / fixed.len() as f32;
        eprintln!(
            "raw MAE {mr:.1} ct · fixed median {mf:.1} p90 {p90:.1} MAE {mae:.1} · score {:.2}",
            t.score
        );
        assert!(
            mf < 20.0 && p90 < 50.0,
            "median {mf:.1}, p90 {p90:.1} cents"
        );
        assert!(mae < mr * 0.5, "MAE {mae:.1} vs raw {mr:.1}");
    }

    #[test]
    fn alignment_finds_the_matching_moments() {
        let s = song(3, 24);
        let take = sing(&s, &Voice::default(), &[], 0.0, SR);
        let reference = sing(&s, &reference_voice(), &[], 0.0, SR);
        let (_, t) = run(&take, &reference, &params());
        let errs: Vec<f32> = take
            .onsets
            .iter()
            .zip(&reference.onsets)
            .filter_map(|(&a, &b)| {
                let f = (a * t.rate as f64).round() as usize;
                let got = *t.ref_time.get(f.checked_sub(t.start)?)?;
                (!got.is_nan()).then(|| ((got as f64 - b).abs() * 1000.0) as f32)
            })
            .collect();
        let med = quantile(&errs, 0.5);
        eprintln!(
            "boundary error median {med:.0} ms, p90 {:.0} ms",
            quantile(&errs, 0.9)
        );
        assert!(med < 60.0, "median boundary error {med:.0} ms");
    }

    #[test]
    fn an_unrelated_reference_changes_nothing() {
        let take = sing(&song(7, 24), &Voice::default(), &[60.0; 24], 0.0, SR);
        let other = sing(&song(1234, 24), &reference_voice(), &[], 0.0, SR);
        let (_, t) = run(&take, &other, &params());
        eprintln!("unrelated score {:.2}", t.score);
        assert!(t.reason.is_some(), "score {:.2}", t.score);
        assert!(t
            .source_f0
            .iter()
            .zip(&t.target_f0)
            .all(|(a, b)| (a.is_nan() && b.is_nan()) || a == b));
    }

    #[test]
    fn the_singers_vibrato_is_kept() {
        let s = song(11, 20);
        let v = Voice { vib_rate: 5.0, vib_depth: 0.5, ..Voice::default() };
        let take = sing(&s, &v, &[40.0; 20], 0.0, SR);
        let reference = sing(&s, &reference_voice(), &[], 0.0, SR);
        let (_, t) = run(&take, &reference, &params());
        // vibrato rate from zero crossings of the pitch around its note centre, over long notes
        let rate = |c: &[f32]| -> f32 {
            let mut rates = vec![];
            for &(a, b, _) in take.notes.iter().filter(|n| n.1 - n.0 > 0.6) {
                let (fa, fb) = (
                    ((a + 0.25) * t.rate as f64) as usize - t.start,
                    ((b - 0.05) * t.rate as f64) as usize - t.start,
                );
                let seg: Vec<f32> = c[fa..fb].to_vec();
                if seg.iter().any(|x| x.is_nan()) {
                    continue;
                }
                let m = seg.iter().sum::<f32>() / seg.len() as f32;
                let zc = seg
                    .windows(2)
                    .filter(|w| (w[0] - m) * (w[1] - m) < 0.0)
                    .count();
                rates.push(zc as f32 / 2.0 / (seg.len() as f32 / t.rate));
            }
            rates.iter().sum::<f32>() / rates.len().max(1) as f32
        };
        let (r0, r1) = (rate(&t.source_f0), rate(&t.target_f0));
        eprintln!("vibrato {r0:.2} Hz → {r1:.2} Hz");
        assert!(r0 > 4.0 && (r0 - r1).abs() < 0.5, "{r0:.2} → {r1:.2} Hz");
    }

    #[test]
    fn the_rendered_take_sings_the_reference_melody() {
        let s = song(21, 16);
        let detune: Vec<f32> = (0..s.len())
            .map(|k| [60.0, -45.0, 90.0, -30.0, 120.0, -80.0][k % 6])
            .collect();
        let take = sing(&s, &Voice::default(), &detune, 0.0, SR);
        let reference = sing(&s, &reference_voice(), &[], 0.0, SR);
        let audio = |r: &Rendition| {
            Audio::from_stereo(r.audio.iter().flat_map(|&x| [x, x]).collect(), SR as u32, 1)
        };
        let (out, t, ta) = tune_audio(&audio(&take), &audio(&reference), &params(), None);
        assert_eq!(out.frames, take.audio.len());
        assert!(t.reason.is_none(), "rejected: {:?}", t.reason);
        let oa = analyze(&mono_of(&out), SR);
        let per_note = |c: &[f32]| -> Vec<f32> {
            take.notes
                .iter()
                .filter_map(|&(a, b, m)| {
                    let v: Vec<f32> = c
                        [((a + 0.06) * 200.0) as usize..((b - 0.04) * 200.0) as usize]
                        .iter()
                        .copied()
                        .filter(|x| !x.is_nan())
                        .collect();
                    median(v)
                        .map(|p| ((p - m) * 100.0).abs())
                        .filter(|e| *e < 600.0)
                })
                .collect()
        };
        let (before, after) = (per_note(&ta.curve), per_note(&oa.curve));
        let (mb, ma) = (quantile(&before, 0.5), quantile(&after, 0.5));
        eprintln!(
            "rendered note error median {mb:.1} → {ma:.1} ct, worst {:.1} ct",
            quantile(&after, 1.0)
        );
        assert!(
            ma < 20.0 && ma < mb * 0.5,
            "median note error {mb:.1} → {ma:.1} cents"
        );
        // voicing is the take's: PSOLA leaves unvoiced sound alone
        let flips = ta
            .curve
            .iter()
            .zip(&oa.curve)
            .filter(|(a, b)| a.is_nan() != b.is_nan())
            .count();
        let frac = flips as f32 / ta.curve.len() as f32;
        eprintln!("voiced/unvoiced flips {:.2} %", frac * 100.0);
        assert!(
            frac < 0.01,
            "{:.2} % of frames changed voicing",
            frac * 100.0
        );
    }

    #[test]
    fn correction_and_influence_blend() {
        let s = song(5, 16);
        let take = sing(&s, &Voice::default(), &[70.0; 16], 0.0, SR);
        let reference = sing(&s, &reference_voice(), &[], 0.0, SR);
        let mut p = params();
        p.correction = 0.0;
        let (_, t) = run(&take, &reference, &p);
        assert!(
            t.source_f0
                .iter()
                .zip(&t.target_f0)
                .all(|(a, b)| (a.is_nan() && b.is_nan()) || (a - b).abs() < 1e-4),
            "0 % correction is the take"
        );
        p.correction = 0.5;
        let (_, half) = run(&take, &reference, &p);
        let e = pitch_errors(&take.truth, &take.edge, &half.target_f0, half.start);
        let med = quantile(&e, 0.5);
        assert!(
            (med - 35.0).abs() < 12.0,
            "half correction of +70 ct leaves ~35 ct, got {med:.1}"
        );
        // influence 0: nearest scale note (C major here = the melody's own notes)
        p.correction = 1.0;
        p.influence = 0.0;
        p.scale = 1;
        let (_, sc) = run(&take, &reference, &p);
        let e = pitch_errors(&take.truth, &take.edge, &sc.target_f0, sc.start);
        assert!(
            quantile(&e, 0.5) < 20.0,
            "scale snap median {:.1}",
            quantile(&e, 0.5)
        );
    }
}
