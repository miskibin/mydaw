//! Offline rendering of a clip's pitch edits (the note editor).
//!
//! Inputs: the source audio, its pitch analysis, the clip's `ClipTune` and, with
//! Tune to Reference, the target derived from the reference. The base pitch is the
//! reference target where there is one, else the sung pitch. Each note then moves by
//! `shift`, its internal drift/vibrato is flattened by `drift`, its level and
//! formants change; pencil strokes replace the pitch where drawn. The pitch change
//! is a continuous function of time (smoothed over 35 ms), so note edges never
//! jump. Unvoiced sound (sibilants, breaths) is passed through untouched because
//! PSOLA is transparent there.

use crate::model::ClipTune;
use crate::pitch::{midi_to_hz, PitchAnalysis, Psola};
use crate::reference::{lerp_at, smooth, PitchTarget};
use crate::sources::Audio;

fn note_center(curve: &[f32], rate: f32, start: f64, end: f64) -> Option<f32> {
    let r = rate as f64;
    let (i0, i1) = ((start * r) as usize, ((end * r) as usize).min(curve.len()));
    let mut v: Vec<f32> = curve.get(i0..i1)?.iter().copied().filter(|x| !x.is_nan()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|x, y| x.total_cmp(y));
    Some(v[v.len() / 2])
}

/// Pencil value at time `t`, if `t` is inside a drawn run.
fn pencil_at(p: &[[f32; 2]], t: f32) -> Option<f32> {
    if p.len() < 2 {
        return None;
    }
    let i = p.partition_point(|q| q[0] <= t);
    if i == 0 || i >= p.len() {
        return None;
    }
    let (a, b) = (p[i - 1], p[i]);
    if b[0] - a[0] > 0.03 {
        return None;
    }
    let f = (t - a[0]) / (b[0] - a[0]).max(1e-6);
    Some(a[1] + (b[1] - a[1]) * f)
}

/// Retune speed (ms) in force at `t`: the note's own, else the reference's; `None` = immediate.
/// A pencil stroke is drawn exactly as wanted, so it is immediate too.
fn speed_at(tune: &ClipTune, ni: usize, t: f64) -> Option<f32> {
    if pencil_at(&tune.pencil, t as f32).is_some() {
        return None;
    }
    let own = tune.notes.get(ni).filter(|n| n.start <= t && t < n.end).and_then(|n| n.speed);
    own.or_else(|| tune.reference.as_ref().and_then(|r| r.speed)).filter(|s| *s > 0.0)
}

/// A retune speed this short (ms) is the hard-tune effect: its steps are kept nearly sharp.
pub const HARD_MS: f32 = 5.0;

/// Per analysis frame: whether the hard-tune smoothing (2.5 ms instead of 17.5 ms) applies.
fn hard_frames(a: &PitchAnalysis, tune: &ClipTune) -> Vec<bool> {
    let mut ni = 0usize;
    (0..a.curve.len())
        .map(|f| {
            let t = f as f64 / a.rate as f64;
            while ni < tune.notes.len() && tune.notes[ni].end <= t {
                ni += 1;
            }
            let own = tune.notes.get(ni).filter(|n| n.start <= t).and_then(|n| n.speed);
            own.or_else(|| tune.reference.as_ref().and_then(|r| r.speed)).is_some_and(|s| s < HARD_MS)
        })
        .collect()
}

/// The pitch each analysis frame will be rendered at (NaN = unvoiced, untouched).
///
/// The correction (wanted pitch − sung pitch) follows its target with the retune speed of
/// the note (Auto-Tune's Retune Speed): a one-pole lag reaching 95 % of a change in `speed`
/// ms. After a breath or consonant it starts from no correction, so the singer's own attack
/// is heard and pulled into tune; without speeds the correction is immediate (bit-identical
/// to projects made before speeds existed).
pub fn edited_curve(a: &PitchAnalysis, tune: &ClipTune, target: Option<&PitchTarget>) -> Vec<f32> {
    let base: Vec<f32> = (0..a.curve.len())
        .map(|f| if a.curve[f].is_nan() { f32::NAN } else { target.and_then(|t| t.at(f)).unwrap_or(a.curve[f]) })
        .collect();
    let centers: Vec<Option<f32>> = tune.notes.iter().map(|nt| note_center(&base, a.rate, nt.start, nt.end)).collect();
    let mut ni = 0usize;
    let mut want: Vec<f32> = (0..base.len())
        .map(|f| {
            let b = base[f];
            if b.is_nan() {
                return f32::NAN;
            }
            let t = f as f64 / a.rate as f64;
            while ni < tune.notes.len() && tune.notes[ni].end <= t {
                ni += 1;
            }
            let mut v = b;
            if let Some(nt) = tune.notes.get(ni).filter(|nt| nt.start <= t) {
                v = match centers[ni] {
                    Some(c) => c + (b - c) * (1.0 - nt.drift.clamp(0.0, 1.0)) + nt.shift / 100.0,
                    None => b + nt.shift / 100.0,
                };
            }
            pencil_at(&tune.pencil, t as f32).unwrap_or(v)
        })
        .collect();
    let timed = tune.notes.iter().any(|n| n.speed.is_some()) || tune.reference.as_ref().is_some_and(|r| r.speed.is_some());
    if timed {
        let dt = 1.0 / a.rate;
        let (mut d, mut ni) = (0f32, 0usize);
        for (f, w) in want.iter_mut().enumerate() {
            if w.is_nan() {
                d = 0.0;
                continue;
            }
            let t = f as f64 / a.rate as f64;
            while ni < tune.notes.len() && tune.notes[ni].end <= t {
                ni += 1;
            }
            let goal = *w - a.curve[f];
            d = match speed_at(tune, ni, t) {
                // 95 % in `ms`: three time constants
                Some(ms) => d + (goal - d) * (1.0 - (-dt * 3000.0 / ms).exp()),
                None => goal,
            };
            *w = a.curve[f] + d;
        }
    }
    want
}

/// The pitch change (semitones) rendered per analysis frame: the edited curve minus the sung
/// one, smoothed over 35 ms so nothing clicks — 5 ms where a hard retune speed asks for steps.
pub fn pitch_change(a: &PitchAnalysis, tune: &ClipTune, target: Option<&PitchTarget>) -> Vec<f32> {
    let want = edited_curve(a, tune, target);
    let delta: Vec<f32> = want.iter().zip(&a.curve).map(|(w, o)| w - o).collect();
    let hard = hard_frames(a, tune);
    if hard.iter().any(|h| *h) {
        let (soft, sharp) = (smooth(&delta, (0.0175 * a.rate) as usize), smooth(&delta, ((0.0025 * a.rate) as usize).max(1)));
        soft.into_iter().zip(sharp).zip(&hard).map(|((s, h), k)| if *k { h } else { s }).collect()
    } else {
        smooth(&delta, (0.0175 * a.rate) as usize)
    }
}

/// Renders `audio` with `tune` applied. Same length and rate as the input.
pub fn render(audio: &Audio, a: &PitchAnalysis, tune: &ClipTune, target: Option<&PitchTarget>) -> Audio {
    let sr = audio.sr as f32;
    let n = audio.frames;
    let src = audio.content();
    let delta = pitch_change(a, tune, target);
    // nothing to change: the take itself, bit for bit (PSOLA at ratio 1 is close, not equal)
    if delta.iter().all(|d| d.is_nan() || d.abs() < 1e-4) && tune.notes.iter().all(|nt| nt.formant == 0.0 && nt.gain == 0.0) {
        return Audio::from_stereo(src.to_vec(), audio.sr, audio.channels);
    }
    let mut ps = [Psola::new(sr), Psola::new(sr)];
    let lat = ps[0].latency();
    let mut out = vec![0f32; n * 2];
    let mut note_i = 0usize;
    let fade = 0.01f64; // 10 ms gain ramps at note edges
    for i in 0..n + lat {
        let t = i as f64 / sr as f64;
        let (x0, x1) = if i < n { (src[i * 2], src[i * 2 + 1]) } else { (0.0, 0.0) };
        let fi = (t * a.rate as f64) as f32;
        let orig = lerp_at(&a.curve, fi);
        while note_i < tune.notes.len() && tune.notes[note_i].end <= t {
            note_i += 1;
        }
        let note = tune.notes.get(note_i).filter(|nt| nt.start <= t);
        let (mut period, mut ratio, mut formant, mut gain) = (0f32, 1f32, 1f32, 1f32);
        if !orig.is_nan() {
            period = sr / midi_to_hz(orig);
            let d = lerp_at(&delta, fi);
            if !d.is_nan() {
                ratio = 2f32.powf(d / 12.0);
            }
            if let Some(nt) = note {
                formant = 2f32.powf(nt.formant / 1200.0);
            }
        }
        if let Some(nt) = note {
            if nt.gain != 0.0 {
                let g = 10f32.powf(nt.gain / 20.0);
                let edge = ((t - nt.start).min(nt.end - t) / fade).clamp(0.0, 1.0) as f32;
                gain = 1.0 + (g - 1.0) * edge;
            }
        }
        let y0 = ps[0].process(x0 * gain, period, ratio, formant);
        let y1 = ps[1].process(x1 * gain, period, ratio, formant);
        if i >= lat {
            let o = i - lat;
            out[o * 2] = y0;
            out[o * 2 + 1] = y1;
        }
    }
    Audio::from_stereo(out, audio.sr, audio.channels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NoteEdit;
    use crate::pitch::{analyze, Detector};

    fn voice(hz: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (1..8).map(|k| (std::f32::consts::TAU * hz * k as f32 * i as f32 / sr).sin() / k as f32).sum::<f32>() * 0.3)
            .collect()
    }

    #[test]
    fn shifting_a_note_moves_only_that_note() {
        let sr = 48000.0;
        let mut mono = voice(220.0, sr, 24000);
        mono.extend(vec![0.0; 4800]);
        mono.extend(voice(330.0, sr, 24000));
        let st: Vec<f32> = mono.iter().flat_map(|&x| [x, x]).collect();
        let audio = Audio::from_stereo(st, 48000, 1);
        let a = analyze(&mono, sr);
        assert_eq!(a.notes.len(), 2);
        let mut tune = ClipTune {
            notes: a
                .notes
                .iter()
                .map(|n| NoteEdit { start: n.start, end: n.end, shift: 0.0, drift: 0.0, formant: 0.0, gain: 0.0, speed: None })
                .collect(),
            pencil: vec![],
            reference: None,
        };
        tune.notes[0].shift = 200.0; // first note up a whole tone
        let out = render(&audio, &a, &tune, None);
        let pitch = |from: usize, to: usize| {
            let mut d = Detector::new(sr);
            for f in from..to {
                d.push(out.content()[f * 2]);
            }
            d.hz
        };
        let p1 = pitch(4000, 22000);
        let p2 = pitch(34000, 50000);
        let c1 = 1200.0 * (p1 / (220.0 * 2f32.powf(2.0 / 12.0))).log2();
        let c2 = 1200.0 * (p2 / 330.0).log2();
        assert!(c1.abs() < 20.0, "first note at {p1} Hz ({c1:.0} cents off)");
        assert!(c2.abs() < 15.0, "second note moved to {p2} Hz ({c2:.0} cents)");
        assert_eq!(out.frames, audio.frames);
    }

    #[test]
    fn the_pitch_change_never_steps_at_a_note_edge() {
        // legato: two touching notes shifted in opposite directions
        let sr = 48000.0;
        let mono = voice(220.0, sr, 48000);
        let a = analyze(&mono, sr);
        let mid = 0.5;
        let edit = |start, end, shift| NoteEdit { start, end, shift, drift: 0.0, formant: 0.0, gain: 0.0, speed: None };
        let tune = ClipTune { notes: vec![edit(0.0, mid, 150.0), edit(mid, 1.0, -150.0)], pencil: vec![], reference: None };
        let want = edited_curve(&a, &tune, None);
        let delta: Vec<f32> = want.iter().zip(&a.curve).map(|(w, o)| w - o).collect();
        let d = smooth(&delta, (0.0175 * a.rate) as usize);
        let steps: f32 = d.windows(2).filter(|w| !w[0].is_nan() && !w[1].is_nan()).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        // 3 semitones spread over 35 ms (7 frames): no frame-to-frame step above half a semitone
        assert!(steps < 0.5, "largest step {steps:.2} semitones per 5 ms");
    }

    #[test]
    fn retune_speed_sets_how_fast_a_note_reaches_its_correction() {
        let sr = 48000.0;
        let mono = voice(220.0, sr, 48000);
        let a = analyze(&mono, sr);
        let note = |speed| ClipTune {
            notes: vec![NoteEdit { start: 0.0, end: 1.0, shift: 100.0, drift: 0.0, formant: 0.0, gain: 0.0, speed }],
            pencil: vec![],
            reference: None,
        };
        let first = a.curve.iter().position(|x| !x.is_nan()).unwrap();
        let at = |c: &[f32], ms: f32| c[first + (ms / 1000.0 * a.rate) as usize] - a.curve[first + (ms / 1000.0 * a.rate) as usize];
        // no speed: the whole correction at once (as before speeds existed)
        let now = edited_curve(&a, &note(None), None);
        assert!((at(&now, 5.0) - 1.0).abs() < 1e-4);
        // 100 ms: under half way after 20 ms, 95 % after 100 ms
        let slow = edited_curve(&a, &note(Some(100.0)), None);
        assert!(at(&slow, 20.0) < 0.6, "{} after 20 ms", at(&slow, 20.0));
        let s100 = at(&slow, 100.0);
        assert!((0.9..=0.97).contains(&s100), "{s100} after 100 ms");
        assert!(at(&slow, 300.0) > 0.999);
    }

    #[test]
    fn hard_tune_keeps_its_steps() {
        let sr = 48000.0;
        let mono = voice(220.0, sr, 48000);
        let a = analyze(&mono, sr);
        let edit = |start, end, shift, speed| NoteEdit { start, end, shift, drift: 0.0, formant: 0.0, gain: 0.0, speed };
        let largest_step = |speed| {
            let tune =
                ClipTune { notes: vec![edit(0.0, 0.5, 150.0, speed), edit(0.5, 1.0, -150.0, speed)], pencil: vec![], reference: None };
            pitch_change(&a, &tune, None)
                .windows(2)
                .filter(|w| !w[0].is_nan() && !w[1].is_nan())
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f32::max)
        };
        // 3 semitones: spread over 7 frames normally, over 3 with hard tune
        let (soft, hard) = (largest_step(None), largest_step(Some(0.0)));
        assert!(soft < 0.5, "soft edge steps {soft:.2} st per frame");
        assert!(hard > 0.9, "hard edge steps only {hard:.2} st per frame");
    }
}
