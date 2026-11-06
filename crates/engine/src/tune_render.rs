//! Offline rendering of a clip's pitch edits (the note editor).
//!
//! Inputs: the source audio, its pitch analysis, and the clip's `ClipTune`.
//! Each note moves by `shift`, its internal drift/vibrato is flattened by
//! `drift`, its level and formants change; pencil strokes replace the pitch
//! where drawn. Unvoiced sound (sibilants, breaths) is passed through untouched
//! because PSOLA is transparent there.

use crate::model::ClipTune;
use crate::pitch::{midi_to_hz, PitchAnalysis, Psola};
use crate::sources::Audio;

fn note_center(a: &PitchAnalysis, start: f64, end: f64) -> Option<f32> {
    let r = a.rate as f64;
    let (i0, i1) = ((start * r) as usize, ((end * r) as usize).min(a.curve.len()));
    let mut v: Vec<f32> = a.curve.get(i0..i1)?.iter().copied().filter(|x| !x.is_nan()).collect();
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

/// Renders `audio` with `tune` applied. Same length and rate as the input.
pub fn render(audio: &Audio, a: &PitchAnalysis, tune: &ClipTune) -> Audio {
    let sr = audio.sr as f32;
    let n = audio.frames;
    let src = audio.content();
    let centers: Vec<Option<f32>> = tune.notes.iter().map(|nt| note_center(a, nt.start, nt.end)).collect();
    let mut ps = [Psola::new(sr), Psola::new(sr)];
    let lat = ps[0].latency();
    let mut out = vec![0f32; n * 2];
    let mut note_i = 0usize;
    let fade = 0.01f64; // 10 ms gain ramps at note edges
    for i in 0..n + lat {
        let t = i as f64 / sr as f64;
        let (x0, x1) = if i < n { (src[i * 2], src[i * 2 + 1]) } else { (0.0, 0.0) };
        // original pitch at t
        let fi = t * a.rate as f64;
        let k = fi as usize;
        let orig = match (a.curve.get(k), a.curve.get(k + 1)) {
            (Some(&p0), Some(&p1)) if !p0.is_nan() && !p1.is_nan() => p0 + (p1 - p0) * (fi - k as f64) as f32,
            (Some(&p0), _) if !p0.is_nan() => p0,
            _ => f32::NAN,
        };
        while note_i < tune.notes.len() && tune.notes[note_i].end <= t {
            note_i += 1;
        }
        let note = tune.notes.get(note_i).filter(|nt| nt.start <= t);
        let (mut period, mut ratio, mut formant, mut gain) = (0f32, 1f32, 1f32, 1f32);
        if !orig.is_nan() {
            period = sr / midi_to_hz(orig);
            let mut target = orig;
            if let Some(nt) = note {
                if let Some(c) = centers[note_i] {
                    target = c + (orig - c) * (1.0 - nt.drift.clamp(0.0, 1.0)) + nt.shift / 100.0;
                } else {
                    target = orig + nt.shift / 100.0;
                }
                formant = 2f32.powf(nt.formant / 1200.0);
            }
            if let Some(pv) = pencil_at(&tune.pencil, t as f32) {
                target = pv;
            }
            ratio = 2f32.powf((target - orig) / 12.0);
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
        (0..n).map(|i| (1..8).map(|k| (std::f32::consts::TAU * hz * k as f32 * i as f32 / sr).sin() / k as f32).sum::<f32>() * 0.3).collect()
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
        let mut tune = ClipTune { notes: a.notes.iter().map(|n| NoteEdit { start: n.start, end: n.end, shift: 0.0, drift: 0.0, formant: 0.0, gain: 0.0 }).collect(), pencil: vec![] };
        tune.notes[0].shift = 200.0; // first note up a whole tone
        let out = render(&audio, &a, &tune);
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
}
