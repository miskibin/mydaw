//! A melody from the project as the reference of Tune to Reference: "sing what I played".
//!
//! The singer sang along with notes the playlist plays, so where each note is in the take is
//! known from the timeline: no alignment has to be guessed (unlike a reference recording).
//! The take keeps its own timing — note edges come from the voice, only the pitch comes from
//! the melody — and a few things a sung line does differently from a keyboard part are
//! absorbed: singing slightly early or late (slivers of a neighbouring note shorter than
//! [`TOLERANCE`] belong to the note sung), chords (the chord note nearest the voice), a melody
//! played in another octave (the octave nearest the voice), ad-libs where nothing is played
//! (left as sung) and one sung note over several played ones (it moves with the melody, at the
//! take's own transition).

use crate::model::{Clip, ClipKind, Id, MelodyNote, Project};
use crate::pitch::PitchAnalysis;
use crate::reference::Alignment;

/// Timing slack between the voice and the melody (seconds).
pub const TOLERANCE: f64 = 0.15;

/// The notes of channel `channel` the playlist plays while `clip` plays, on the clip's source
/// time axis (seconds), clipped to the clip's region; `span` keeps only notes overlapping it.
/// Muted pattern clips are left out. Sorted by start, duplicates (one pattern on two tracks)
/// removed.
pub fn collect(p: &Project, clip: &Clip, channel: Id, span: Option<[f64; 2]>) -> Vec<MelodyNote> {
    let spb = 60.0 / p.bpm.max(1.0);
    let st = clip.stretch.max(crate::model::STRETCH_MIN);
    let root = p.channels.iter().find(|c| c.id == channel).map(|c| c.root);
    let Some(root) = root else { return vec![] };
    let (r0, r1) = clip.source_span(p.bpm);
    let to_src = |beat: f64| (beat - clip.start + clip.offset) * spb / st;
    let mut out: Vec<MelodyNote> = vec![];
    for pc in p.clips.iter().filter(|c| c.kind == ClipKind::Pat && !c.muted) {
        let lo = pc.start.max(clip.start);
        let hi = (pc.start + pc.len).min(clip.start + clip.len);
        if hi <= lo {
            continue;
        }
        let Some(pat) = p.patterns.iter().find(|x| x.id == pc.source) else { continue };
        if pat.len <= 0.0 {
            continue;
        }
        let base = pc.start - pc.offset;
        let end_steps = (pat.len * crate::midi::STEPS_PER_BEAT) as f32;
        // the same repeats the sequencer plays (rt.rs), notes past the pattern end left out
        let (k0, k1) = (((lo - base) / pat.len).floor() as i64 - 1, ((hi - base) / pat.len).floor() as i64);
        for k in k0..=k1 {
            for n in pat.notes.iter().filter(|n| n.ch == channel && n.step < end_steps - 1e-4) {
                let step = n.step + crate::model::swing_offset(n.step, pat.swing);
                let nb = base + k as f64 * pat.len + step as f64 / crate::midi::STEPS_PER_BEAT;
                if nb < pc.start || nb >= pc.start + pc.len {
                    continue; // the clip does not play this repeat's note
                }
                let ne = (nb + n.len as f64 / crate::midi::STEPS_PER_BEAT).min(pc.start + pc.len);
                let (a, b) = (to_src(nb).max(r0), to_src(ne).min(r1));
                if b - a < 0.01 {
                    continue;
                }
                if let Some([s0, s1]) = span {
                    if b <= s0 || a >= s1 {
                        continue;
                    }
                }
                out.push(MelodyNote { start: a, end: b, key: n.p.unwrap_or(root) });
            }
        }
    }
    out.sort_by(|x, y| x.start.total_cmp(&y.start).then(x.key.cmp(&y.key)));
    out.dedup_by(|x, y| (x.start - y.start).abs() < 1e-6 && x.key == y.key);
    out
}

/// Channels that play notes over `clip` (pattern clips only), with how many.
pub fn channels_over(p: &Project, clip: &Clip) -> Vec<(Id, usize)> {
    let mut out: Vec<(Id, usize)> = p.channels.iter().map(|c| (c.id, collect(p, clip, c.id, None).len())).filter(|x| x.1 > 0).collect();
    out.sort_by_key(|x| std::cmp::Reverse(x.1));
    out
}

/// How well `notes` fit what is sung (0..1): the share of sung frames over a played note whose
/// pitch class is within a semitone of one of the notes sounding there.
pub fn fit(take: &PitchAnalysis, region: (f64, f64), notes: &[MelodyNote]) -> f32 {
    let r = take.rate as f64;
    let (f0, f1) = (((region.0 * r) as usize).min(take.curve.len()), ((region.1 * r) as usize).min(take.curve.len()));
    let (mut sung, mut hit) = (0usize, 0usize);
    let mut i0 = 0usize;
    for f in f0..f1 {
        let u = take.curve[f];
        if u.is_nan() {
            continue;
        }
        sung += 1;
        let t = f as f64 / r;
        while i0 < notes.len() && notes[i0].end + 2.0 < t {
            i0 += 1;
        }
        let near = notes[i0..].iter().take_while(|n| n.start <= t).filter(|n| n.end > t).any(|n| {
            let d = (u - n.key as f32).rem_euclid(12.0);
            d.min(12.0 - d) <= 1.0
        });
        if near {
            hit += 1;
        }
    }
    if sung == 0 {
        0.0
    } else {
        hit as f32 / sung as f32
    }
}

fn median(mut v: Vec<f32>) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

/// One stretch of a sung note and the melody key it takes (`None` = nothing played there).
#[derive(Clone, Copy, Debug)]
struct Seg {
    a: f64,
    b: f64,
    key: Option<f32>,
}

/// An "alignment" of the take to the melody, for [`crate::reference::target`]: the melody's
/// pitch (untransposed) on every sung frame of the region that a played note covers, full
/// confidence there and none elsewhere, the octave nearest the voice, and as the score the
/// share of sung frames the melody covers. `spans` are the take's notes (the editor's
/// segmentation). `reason` is set when nothing is played while the take is sung.
pub fn alignment(take: &PitchAnalysis, region: (f64, f64), spans: &[(f64, f64)], notes: &[MelodyNote]) -> Alignment {
    let rate = take.rate as f64;
    let s = ((region.0 * rate).round() as usize).min(take.curve.len());
    let e = ((region.1 * rate).round() as usize).clamp(s, take.curve.len());
    let n = e - s;
    let u = |t: f64| take.curve.get((t * rate) as usize).copied().unwrap_or(f32::NAN);
    let sung_median = |a: f64, b: f64| median(((a * rate).ceil() as usize..((b * rate).ceil() as usize).min(take.curve.len())).map(|f| take.curve[f]).filter(|x| !x.is_nan()).collect());
    let sounding = |a: f64, b: f64| -> Vec<&MelodyNote> { notes.iter().filter(|m| m.start < b && m.end > a).collect() };
    let spans: Vec<(f64, f64)> = spans.iter().map(|&(a, b)| (a.max(region.0), b.min(region.1))).filter(|(a, b)| b > a).collect();

    // the octave: the multiple of 12 bringing the played notes nearest the sung ones
    let mut votes: Vec<(f32, f32, Vec<f32>)> = vec![]; // (sung centre, weight, keys sounding)
    for &(a, b) in &spans {
        let Some(c) = sung_median(a, b) else { continue };
        let ks: Vec<f32> = sounding(a, b).iter().map(|m| m.key as f32).collect();
        if !ks.is_empty() {
            votes.push((c, (b - a) as f32, ks));
        }
    }
    let octave = (-4..=4)
        .map(|k| k * 12)
        .min_by(|&x, &y| {
            let cost = |o: i32| votes.iter().map(|(c, w, ks)| w * ks.iter().map(|k| (c - (k + o as f32)).abs()).fold(f32::INFINITY, f32::min)).sum::<f32>();
            cost(x).total_cmp(&cost(y)).then(x.abs().cmp(&y.abs()))
        })
        .unwrap_or(0);

    let mut reference_f0 = vec![f32::NAN; n];
    for &(a, b) in &spans {
        // stretches of the note between the played notes' edges, each with the chord note nearest the voice
        let mut edges: Vec<f64> = vec![a, b];
        for m in sounding(a, b) {
            for x in [m.start, m.end] {
                if x > a && x < b {
                    edges.push(x);
                }
            }
        }
        edges.sort_by(f64::total_cmp);
        edges.dedup_by(|x, y| (*x - *y).abs() < 1e-6);
        let whole = sung_median(a, b);
        let mut segs: Vec<Seg> = vec![];
        for w in edges.windows(2) {
            let (sa, sb) = (w[0], w[1]);
            let c = sung_median(sa, sb).or(whole);
            let key = sounding(sa, sb)
                .iter()
                .map(|m| m.key as f32)
                .min_by(|x, y| match c {
                    Some(c) => (c - (x + octave as f32)).abs().total_cmp(&(c - (y + octave as f32)).abs()),
                    None => y.total_cmp(x), // nothing sung to compare: the top note
                });
            match segs.last_mut() {
                Some(l) if l.key == key => l.b = sb,
                _ => segs.push(Seg { a: sa, b: sb, key }),
            }
        }
        // timing slack: a sliver shorter than the tolerance takes its longer neighbour's key
        while let Some(i) = (0..segs.len()).filter(|&i| segs.len() > 1 && segs[i].b - segs[i].a < TOLERANCE).min_by(|&i, &j| (segs[i].b - segs[i].a).total_cmp(&(segs[j].b - segs[j].a))) {
            let left = i.checked_sub(1).map(|j| (j, segs[j].b - segs[j].a));
            let right = segs.get(i + 1).map(|x| (i + 1, x.b - x.a));
            let into = match (left, right) {
                (Some(l), Some(r)) => if l.1 >= r.1 { l.0 } else { r.0 },
                (Some(l), None) => l.0,
                (None, Some(r)) => r.0,
                (None, None) => break,
            };
            let (lo, hi) = (segs[i].a.min(segs[into].a), segs[i].b.max(segs[into].b));
            segs[into].a = lo;
            segs[into].b = hi;
            segs.remove(i);
            // neighbours that now carry the same key are one stretch
            let mut k = 0;
            while k + 1 < segs.len() {
                if segs[k].key == segs[k + 1].key {
                    segs[k].b = segs[k + 1].b;
                    segs.remove(k + 1);
                } else {
                    k += 1;
                }
            }
        }
        // a change of played note inside one sung note (melisma) happens where the voice moves:
        // the steepest pitch change within the tolerance of the played edge
        for k in 0..segs.len().saturating_sub(1) {
            if segs[k].key.is_none() || segs[k + 1].key.is_none() {
                continue;
            }
            let edge = segs[k].b;
            let lo = (edge - TOLERANCE).max(segs[k].a + 0.02);
            let hi = (edge + TOLERANCE).min(segs[k + 1].b - 0.02);
            let mut best = (edge, 0f32);
            let mut t = lo;
            while t <= hi {
                let d = (u(t + 0.01) - u(t - 0.01)).abs();
                if !d.is_nan() && d > best.1 {
                    best = (t, d);
                }
                t += 1.0 / rate;
            }
            if best.1 > 0.3 {
                segs[k].b = best.0;
                segs[k + 1].a = best.0;
            }
        }
        for sg in segs {
            let Some(key) = sg.key else { continue };
            let (fa, fb) = (((sg.a * rate).round() as usize).saturating_sub(s).min(n), ((sg.b * rate).round() as usize).saturating_sub(s).min(n));
            reference_f0[fa..fb].fill(key);
        }
    }
    let sung: Vec<bool> = (s..e).map(|f| !take.curve[f].is_nan()).collect();
    let confidence: Vec<f32> = (0..n).map(|f| if sung[f] && !reference_f0[f].is_nan() { 1.0 } else { 0.0 }).collect();
    let voiced = sung.iter().filter(|x| **x).count();
    let covered = confidence.iter().filter(|c| **c > 0.0).count();
    let score = if voiced == 0 { 0.0 } else { covered as f32 / voiced as f32 };
    let reason = if notes.is_empty() {
        Some("the melody has no notes over this clip".to_string())
    } else if covered == 0 {
        Some("no melody note plays while this take is sung".to_string())
    } else {
        None
    };
    Alignment {
        start: s,
        ref_time: (s..e).map(|f| f as f32 / take.rate).collect(),
        reference_f0,
        confidence,
        octave,
        score,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clip, ClipTune, Melody, RefTune};
    use crate::pitch::{analyze, midi_to_hz};

    const SR: f32 = 48000.0;

    /// A voice-like tone following `midi(t)` (phase-continuous), silent where it is NaN.
    fn sing(secs: f32, midi: impl Fn(f32) -> f32) -> Vec<f32> {
        let n = (secs * SR) as usize;
        let mut ph = 0f32;
        (0..n)
            .map(|i| {
                let m = midi(i as f32 / SR);
                if m.is_nan() {
                    return 0.0;
                }
                ph = (ph + midi_to_hz(m) / SR).fract();
                (1..8).map(|k| (std::f32::consts::TAU * ph * k as f32).sin() / k as f32).sum::<f32>() * 0.3
            })
            .collect()
    }

    fn params() -> RefTune {
        RefTune { source: 0, correction: 1.0, vibrato: 1.0, influence: 1.0, octave: None, key: 0, scale: 0, speed: None, melody: None }
    }

    fn median_between(x: &[f32], rate: f32, a: f32, b: f32) -> f32 {
        let mut v: Vec<f32> = x[(a * rate) as usize..(b * rate) as usize].iter().copied().filter(|v| !v.is_nan()).collect();
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    }

    #[test]
    fn a_late_sharp_take_lands_on_the_played_notes_in_its_own_octave_and_timing() {
        // sung: A3 +40 c until 0.60 s (the melody moved on at 0.52), C4 +40 c to 1.2 s, a breath,
        // then an ad-lib (E4) with nothing played under it
        let mono = sing(1.7, |t| match t {
            t if t < 0.60 => 57.4,
            t if t < 1.2 => 60.4,
            t if t < 1.3 => f32::NAN,
            _ => 64.3,
        });
        let take = analyze(&mono, SR);
        // played an octave higher: A4, then C5
        let notes = vec![MelodyNote { start: 0.0, end: 0.52, key: 69 }, MelodyNote { start: 0.52, end: 1.2, key: 72 }];
        let spans: Vec<(f64, f64)> = take.notes.iter().map(|n| (n.start, n.end)).collect();
        let al = alignment(&take, (0.0, 1.7), &spans, &notes);
        assert_eq!(al.octave, -12);
        assert!(al.reason.is_none());
        let t = crate::reference::target(&take, &spans, &al, &params());
        let r = take.rate;
        let a = median_between(&t.target_f0, r, 0.1, 0.45);
        let c = median_between(&t.target_f0, r, 0.75, 1.1);
        assert!((a - 57.0).abs() < 0.05, "first note at {a}");
        assert!((c - 60.0).abs() < 0.05, "second note at {c}");
        // the late stretch (0.53–0.57 s) is still the first note: timing is the singer's
        let late = median_between(&t.target_f0, r, 0.53, 0.57);
        assert!((late - 57.0).abs() < 0.1, "late part pulled to {late}");
        // the ad-lib is left exactly as sung
        let f = (1.5 * r) as usize;
        assert_eq!(t.target_f0[f], take.curve[f]);
        assert!(al.score > 0.6 && al.score < 0.9, "score {}", al.score);
    }

    #[test]
    fn a_chord_gives_the_note_nearest_the_voice_and_nothing_played_is_refused() {
        let mono = sing(0.8, |_| 64.3); // E4, a little sharp
        let take = analyze(&mono, SR);
        let spans: Vec<(f64, f64)> = take.notes.iter().map(|n| (n.start, n.end)).collect();
        let chord: Vec<MelodyNote> = [60u8, 64, 67].iter().map(|&k| MelodyNote { start: 0.0, end: 0.8, key: k }).collect();
        let al = alignment(&take, (0.0, 0.8), &spans, &chord);
        let t = crate::reference::target(&take, &spans, &al, &params());
        assert!((median_between(&t.target_f0, take.rate, 0.2, 0.6) - 64.0).abs() < 0.05);
        let none = alignment(&take, (0.0, 0.8), &spans, &[]);
        assert!(none.reason.is_some() && none.confidence.iter().all(|c| *c == 0.0));
        let elsewhere = alignment(&take, (0.0, 0.8), &spans, &[MelodyNote { start: 2.0, end: 3.0, key: 64 }]);
        assert!(elsewhere.reason.is_some());
    }

    #[test]
    fn collect_places_pattern_notes_on_the_takes_source_time() {
        let mut p = Project::empty();
        p.bpm = 120.0; // 0.5 s per beat
        let ch: crate::model::Channel = serde_json::from_value(serde_json::json!({"id": 7001, "name": "Piano", "color": "#fff", "kind": "synth", "root": 60})).unwrap();
        p.channels.push(ch);
        let pat: crate::model::Pattern = serde_json::from_value(serde_json::json!({"id": 7100, "name": "Melody", "color": "#fff", "len": 4.0,
            "notes": [{"ch": 7001, "step": 0, "len": 4, "p": 69}, {"ch": 7001, "step": 8, "len": 8}]})).unwrap();
        p.patterns.push(pat);
        let pc: Clip = serde_json::from_value(serde_json::json!({"id": 7200, "track": 0, "start": 4.0, "len": 8.0, "offset": 0, "kind": "pat", "ref": 7100, "muted": false, "gain": 0})).unwrap();
        p.clips.push(pc);
        // the vocal starts at beat 5, 1 beat into its source, played half as fast (stretch 2)
        let vocal: Clip = serde_json::from_value(serde_json::json!({"id": 7300, "track": 1, "start": 5.0, "len": 6.0, "offset": 1.0, "kind": "audio", "ref": 1, "stretch": 2.0, "muted": false, "gain": 0})).unwrap();
        let notes = collect(&p, &vocal, 7001, None);
        // beats 4–5 (A4, before the vocal), 6–8 (root C4), then the repeat: 8–9 (A4), 10–11 (cut at the clip end 11)
        let src = |b: f64| (b - 5.0 + 1.0) * 0.5 / 2.0;
        let want = [(src(6.0), src(8.0), 60u8), (src(8.0), src(9.0), 69), (src(10.0), src(11.0), 60)];
        assert_eq!(notes.len(), want.len(), "{notes:?}");
        for (n, w) in notes.iter().zip(want) {
            assert!((n.start - w.0).abs() < 1e-9 && (n.end - w.1).abs() < 1e-9 && n.key == w.2, "{n:?} != {w:?}");
        }
        assert_eq!(collect(&p, &vocal, 7001, Some([src(8.0) + 0.01, src(9.0)])).len(), 1);
        assert!(collect(&p, &vocal, 9999, None).is_empty());
        // stored on the clip, a melody reference survives a JSON round trip
        let tune = ClipTune { notes: vec![], pencil: vec![], reference: Some(RefTune { melody: Some(Melody { channel: 7001, span: None, notes }), speed: Some(40.0), ..params() }) };
        let back: ClipTune = serde_json::from_str(&serde_json::to_string(&tune).unwrap()).unwrap();
        assert_eq!(back, tune);
    }
}
