//! Synthetic suite: a deterministic singer sings the same songs twice, the take with
//! known per-note errors (±25…400 cents) or drift, the reference as another voice
//! (other formants, tempo, octave, vibrato). The intended pitch is known exactly.

use std::path::Path;

use wave_engine::pitch::F0Estimator;
use wave_engine::singer::{sing, song, Voice};
use wave_engine::sources::write_wav;
use wave_engine::RefTune;

use crate::eval::{self, mono_audio, Case, Outcome};
use crate::Rng;

pub const LEVELS: [f32; 5] = [25.0, 50.0, 100.0, 200.0, 400.0];
const SR: f32 = 44100.0;

fn take_voice(s: u32) -> Voice {
    Voice { seed: s + 1, vib_rate: 4.8 + 0.3 * (s % 4) as f32, vib_depth: 0.3, ..Voice::default() }
}

fn reference_voice(s: u32) -> Voice {
    match s % 3 {
        0 => Voice { formant: 1.17, tempo: 0.88, jitter: 0.12, transpose: 12.0, vib_rate: 6.2, vib_depth: 0.25, seed: 100 + s },
        1 => Voice { formant: 0.9, tempo: 1.12, jitter: 0.1, transpose: 0.0, vib_rate: 5.0, vib_depth: 0.2, seed: 100 + s },
        _ => Voice { formant: 1.05, tempo: 0.95, jitter: 0.15, transpose: -12.0, vib_rate: 5.8, vib_depth: 0.3, seed: 100 + s },
    }
}

pub fn run(songs: u32, only: Option<&str>, p: &RefTune, est: Option<&dyn F0Estimator>, write: Option<&Path>) -> Vec<Outcome> {
    let mut out = vec![];
    for s in 0..songs {
        let melody = song(100 + s, 24);
        let reference = sing(&melody, &reference_voice(s), &[], 0.0, SR);
        let ref_audio = mono_audio(&reference.audio, SR as u32);
        let mut rng = Rng(7919 * (s + 1));
        let mut cases: Vec<(String, Vec<f32>, f32)> = LEVELS
            .iter()
            .map(|&l| (format!("±{l:.0} ct"), (0..melody.len()).map(|_| if rng.unit() < 0.5 { -l } else { l }).collect(), 0.0))
            .collect();
        cases.push(("drift 40 ct/s".into(), (0..melody.len()).map(|_| rng.sym() * 50.0).collect(), 40.0));
        for (group, detune, drift) in cases {
            if only.is_some_and(|o| !format!("song{s} {group}").contains(o)) {
                continue;
            }
            let take = sing(&melody, &take_voice(s), &detune, drift, SR);
            let take_audio = mono_audio(&take.audio, SR as u32);
            let c = Case {
                name: format!("song{s} {group}"),
                group: group.clone(),
                take: &take_audio,
                reference: &ref_audio,
                truth: take.truth.clone(),
                edge: take.edge.clone(),
                sung: take.sung.clone(),
                notes: take.notes.clone(),
                onsets: take.onsets.iter().copied().zip(reference.onsets.iter().copied()).collect(),
                expect_rejection: false,
            };
            let (o, rendered) = eval::run(&c, p, est);
            if let Some(dir) = write.filter(|_| group == "±100 ct") {
                let _ = std::fs::create_dir_all(dir);
                let _ = write_wav(&dir.join(format!("song{s}_take.wav")), take_audio.content(), SR as u32);
                let _ = write_wav(&dir.join(format!("song{s}_reference.wav")), ref_audio.content(), SR as u32);
                let _ = write_wav(&dir.join(format!("song{s}_tuned.wav")), rendered.content(), SR as u32);
            }
            eprintln!(
                "  {:<24} score {:.2} frame median {:5.1} ct (was {:5.1})",
                o.name,
                o.score,
                eval::quantile(&o.frames_after, 0.5),
                eval::quantile(&o.frames_before, 0.5)
            );
            out.push(o);
        }
        // the same take against a different song: must be left alone
        if only.is_some_and(|o| !format!("song{s} unrelated").contains(o)) {
            continue;
        }
        let other = sing(&song(9000 + s, 24), &reference_voice(s), &[], 0.0, SR);
        let other_audio = mono_audio(&other.audio, SR as u32);
        let take = sing(&melody, &take_voice(s), &[60.0; 24], 0.0, SR);
        let take_audio = mono_audio(&take.audio, SR as u32);
        let c = Case {
            name: format!("song{s} unrelated reference"),
            group: "unrelated".into(),
            take: &take_audio,
            reference: &other_audio,
            truth: vec![],
            edge: vec![],
            sung: take.sung.clone(),
            notes: vec![],
            onsets: vec![],
            expect_rejection: true,
        };
        let (o, _) = eval::run(&c, p, est);
        eprintln!("  {:<24} score {:.2} {}", o.name, o.score, if o.corrected { "CORRECTED (wrong)" } else { "held back" });
        out.push(o);
    }
    out
}
