//! Generated-signal gates for the root-pitch detector.
//!
//! These fixtures are synthesised in-process so the repository never ships
//! licensed audio. They prove the public detector stays inside the approved
//! accuracy bounds on clean one-shots, detuned tones, and unpitched negatives.
//!
//! Optional external corpus: set `MYDAW_PITCH_CORPUS` to a directory that
//! contains `manifest.json`. That runner is ignored by default, never downloads
//! data, and only reads files already on disk.
//!
//! Expected manifest:
//!
//! ```json
//! {
//!   "samples": [
//!     { "file": "guitar_A3.wav", "midiNote": 57, "cents": 0, "pitched": true },
//!     { "file": "hat.wav", "pitched": false }
//!   ]
//! }
//! ```
//!
//! Licensing: do not commit NSynth, MDB-stem-synth, or any other non-owned
//! corpus into this repository. Prepare those locally if you want the ignored
//! runner. NSynth notes are typically named with MIDI numbers; MDB-stem-synth
//! stems should be labelled from the accompanying annotation, not guessed.

use std::f64::consts::TAU;
use std::path::{Path, PathBuf};

use mydaw_pitch_analysis::{detect_root_pitch, NoPitchReason, PitchOutcome};

const RATE: f64 = 44_100.0;

fn stereo_sine(seconds: f64, hz: f64, amplitude: f32) -> Vec<f32> {
    let frames = (seconds * RATE).round() as usize;
    let mut data = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let sample = (TAU * hz * (frame as f64) / RATE).sin() as f32 * amplitude;
        data.push(sample);
        data.push(sample);
    }
    data
}

fn stereo_noise(seconds: f64, seed: u32) -> Vec<f32> {
    let frames = (seconds * RATE).round() as usize;
    let mut state = seed;
    let mut data = Vec::with_capacity(frames * 2);
    for _ in 0..frames {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let sample = ((state >> 8) as f32 / 8_388_607.5 - 1.0) * 0.45;
        data.push(sample);
        data.push(sample);
    }
    data
}

fn midi_hz(note: u8, cents: f64) -> f64 {
    440.0 * 2f64.powf((f64::from(note) - 69.0) / 12.0 + cents / 1_200.0)
}

struct PitchedCase {
    note: u8,
    cents: f64,
}

fn pitched_corpus() -> Vec<PitchedCase> {
    let mut cases = Vec::new();
    for note in 36_u8..=84 {
        if note % 2 == 0 {
            cases.push(PitchedCase { note, cents: 0.0 });
        }
    }
    for (note, cents) in [
        (48_u8, 8.0),
        (55, -12.0),
        (60, 23.0),
        (67, -31.0),
        (72, 15.0),
    ] {
        cases.push(PitchedCase { note, cents });
    }
    cases
}

fn detect(signal: &[f32]) -> PitchOutcome {
    detect_root_pitch(signal, signal.len() / 2, RATE, 0..signal.len() / 2)
}

#[test]
fn generated_corpus_meets_accuracy_gates() {
    let pitched = pitched_corpus();
    let mut exact = 0_usize;
    let mut cents_errors = Vec::new();
    for case in &pitched {
        let hz = midi_hz(case.note, case.cents);
        let signal = stereo_sine(1.0, hz, 0.6);
        match detect(&signal) {
            PitchOutcome::Detected(pitch) => {
                let error = (f64::from(pitch.midi_note) + f64::from(pitch.cents) / 100.0)
                    - (f64::from(case.note) + case.cents / 100.0);
                let cents = error * 100.0;
                if pitch.midi_note == case.note {
                    exact += 1;
                }
                cents_errors.push(cents.abs());
            }
            PitchOutcome::NoStablePitch(_) => cents_errors.push(9_999.0),
        }
    }
    cents_errors.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let exact_rate = exact as f64 / pitched.len() as f64;
    let median = cents_errors[cents_errors.len() / 2];
    let p95_index = ((cents_errors.len() as f64) * 0.95).ceil() as usize - 1;
    let p95 = cents_errors[p95_index.min(cents_errors.len() - 1)];

    let negatives = 40_usize;
    let mut false_positives = 0_usize;
    for seed in 0..negatives {
        let signal = stereo_noise(0.8, 0xA11CE + seed as u32 * 17);
        if matches!(detect(&signal), PitchOutcome::Detected(_)) {
            false_positives += 1;
        }
    }
    let click = stereo_sine(0.03, 1_000.0, 0.9);
    if matches!(detect(&click), PitchOutcome::Detected(_)) {
        false_positives += 1;
    }
    let negative_count = negatives + 1;
    let false_positive_rate = false_positives as f64 / negative_count as f64;

    assert!(
        exact_rate >= 0.95,
        "exact-note accuracy {exact_rate:.3} below 0.95 ({exact}/{})",
        pitched.len()
    );
    assert!(median <= 10.0, "median cents error {median:.2} above 10");
    assert!(p95 <= 35.0, "P95 cents error {p95:.2} above 35");
    assert!(
        false_positive_rate <= 0.02,
        "false-positive rate {false_positive_rate:.3} above 0.02 ({false_positives}/{negative_count})"
    );
}

#[test]
fn generated_silence_is_a_typed_no_result() {
    let signal = vec![0.0; (RATE as usize) * 2];
    assert_eq!(
        detect(&signal),
        PitchOutcome::NoStablePitch(NoPitchReason::Silent)
    );
}

/// External labelled corpus. Ignored unless `MYDAW_PITCH_CORPUS` points at a
/// directory with `manifest.json` and the listed WAV files.
#[test]
#[ignore = "optional local corpus; never downloads"]
fn optional_external_corpus_reports_metrics() {
    let Some(root) = std::env::var_os("MYDAW_PITCH_CORPUS").map(PathBuf::from) else {
        eprintln!("MYDAW_PITCH_CORPUS is unset; nothing to measure");
        return;
    };
    let manifest_path = root.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", manifest_path.display()));
    let manifest: CorpusManifest =
        serde_json::from_str(&text).expect("manifest.json must match the documented shape");
    let started = std::time::Instant::now();
    let mut pitched = 0_usize;
    let mut exact = 0_usize;
    let mut errors = Vec::new();
    let mut negatives = 0_usize;
    let mut false_positives = 0_usize;
    let mut rejected = 0_usize;
    for sample in &manifest.samples {
        let path = root.join(&sample.file);
        let (signal, rate) = read_wav(&path);
        let outcome = detect_root_pitch(&signal, signal.len() / 2, rate, 0..signal.len() / 2);
        if sample.pitched {
            pitched += 1;
            match outcome {
                PitchOutcome::Detected(pitch) => {
                    let expected_note = sample.midi_note.expect("pitched rows need midiNote");
                    let expected_cents = sample.cents.unwrap_or(0.0);
                    if pitch.midi_note == expected_note {
                        exact += 1;
                    }
                    let error = (f64::from(pitch.midi_note) + f64::from(pitch.cents) / 100.0)
                        - (f64::from(expected_note) + expected_cents / 100.0);
                    errors.push(error.abs() * 100.0);
                }
                PitchOutcome::NoStablePitch(_) => rejected += 1,
            }
        } else {
            negatives += 1;
            match outcome {
                PitchOutcome::Detected(_) => false_positives += 1,
                PitchOutcome::NoStablePitch(_) => rejected += 1,
            }
        }
    }
    errors.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = errors.get(errors.len() / 2).copied().unwrap_or(0.0);
    let p95 = if errors.is_empty() {
        0.0
    } else {
        errors[(((errors.len() as f64) * 0.95).ceil() as usize)
            .saturating_sub(1)
            .min(errors.len() - 1)]
    };
    let elapsed = started.elapsed();
    println!(
        "external corpus: exact={}/{} ({:.1}%) median={:.2}ct p95={:.2}ct false_pos={}/{} reject={} elapsed={:.1}ms",
        exact,
        pitched,
        if pitched == 0 {
            0.0
        } else {
            100.0 * exact as f64 / pitched as f64
        },
        median,
        p95,
        false_positives,
        negatives,
        rejected,
        elapsed.as_secs_f64() * 1_000.0
    );
}

#[derive(serde::Deserialize)]
struct CorpusManifest {
    samples: Vec<CorpusRow>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorpusRow {
    file: String,
    #[serde(default)]
    midi_note: Option<u8>,
    #[serde(default)]
    cents: Option<f64>,
    pitched: bool,
}

fn read_wav(path: &Path) -> (Vec<f32>, f64) {
    let mut reader = hound::WavReader::open(path)
        .unwrap_or_else(|err| panic!("could not open {}: {err}", path.display()));
    let spec = reader.spec();
    let rate = f64::from(spec.sample_rate);
    let channels = spec.channels.max(1) as usize;
    let mut frames: Vec<f32> = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                frames.push(sample.unwrap_or(0.0));
            }
        }
        hound::SampleFormat::Int => {
            let max = f32::from(1_i16 << (spec.bits_per_sample.saturating_sub(1).min(15)));
            for sample in reader.samples::<i32>() {
                frames.push(sample.unwrap_or(0) as f32 / max.max(1.0));
            }
        }
    }
    let stereo = if channels == 1 {
        frames
            .iter()
            .flat_map(|sample| [*sample, *sample])
            .collect()
    } else {
        let mut stereo = Vec::with_capacity((frames.len() / channels) * 2);
        for chunk in frames.chunks(channels) {
            stereo.push(chunk[0]);
            stereo.push(*chunk.get(1).unwrap_or(&chunk[0]));
        }
        stereo
    };
    (stereo, rate)
}
