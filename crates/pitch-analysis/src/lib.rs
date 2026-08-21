use std::ops::Range;
use std::path::Path;

use realfft::RealFftPlanner;

const ANALYSIS_RATE: f64 = 12_000.0;
const MIN_SECONDS: f64 = 0.08;
const MAX_SECONDS: f64 = 15.0;
const FRAME_SIZE: usize = 2_048;
const MAX_FRAMES: usize = 96;
const MIN_FREQUENCY: f64 = 40.0;
const MAX_FREQUENCY: f64 = 2_000.0;
const SILENCE_RMS: f64 = 1.0e-4;
const YIN_THRESHOLD: f64 = 0.18;
const MIN_FRAME_CONFIDENCE: f64 = 0.72;
const MIN_VOICED_COVERAGE: f32 = 0.35;
const MAX_STABLE_SPREAD_CENTS: f64 = 42.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PitchSource {
    Metadata,
    Filename,
    Analysis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoPitchReason {
    Silent,
    TooShort,
    Unvoiced,
    Unstable,
    Polyphonic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RootPitch {
    pub midi_note: u8,
    pub cents: f32,
    pub confidence: f32,
    pub voiced_coverage: f32,
    pub source: PitchSource,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PitchOutcome {
    Detected(RootPitch),
    NoStablePitch(NoPitchReason),
}

pub fn root_hint(path: &Path) -> Option<RootPitch> {
    wav_root_hint(path).or_else(|| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(filename_root_hint)
    })
}

fn wav_root_hint(path: &Path) -> Option<RootPitch> {
    const MAX_HEADER_BYTES: u64 = 1024 * 1024;
    use std::io::Read;

    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
    {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_HEADER_BYTES).read_to_end(&mut bytes).ok()?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut cursor = 12_usize;
    while cursor.checked_add(8)? <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().ok()?) as usize;
        let data = cursor.checked_add(8)?;
        let end = data.checked_add(size)?;
        if end > bytes.len() {
            return None;
        }
        if id == b"smpl" && size >= 36 {
            let midi_note = u32::from_le_bytes(bytes[data + 12..data + 16].try_into().ok()?);
            if midi_note > 127 {
                return None;
            }
            let fraction = u32::from_le_bytes(bytes[data + 16..data + 20].try_into().ok()?);
            return Some(RootPitch {
                midi_note: midi_note as u8,
                cents: ((fraction as f64) * (100.0 / 4_294_967_296.0)) as f32,
                confidence: 1.0,
                voiced_coverage: 1.0,
                source: PitchSource::Metadata,
            });
        }
        cursor = end.checked_add(size & 1)?;
    }
    None
}

fn filename_root_hint(stem: &str) -> Option<RootPitch> {
    let chars: Vec<char> = stem.chars().collect();
    for start in 0..chars.len() {
        if start > 0 && chars[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let letter = chars[start].to_ascii_uppercase();
        if !matches!(letter, 'A'..='G') {
            continue;
        }
        let mut cursor = start + 1;
        let mut accidental = 0_i16;
        if let Some(symbol) = chars.get(cursor) {
            if *symbol == '#' {
                accidental = 1;
                cursor += 1;
            } else if *symbol == 'b' || *symbol == 'B' {
                accidental = -1;
                cursor += 1;
            }
        }
        if chars.get(cursor) == Some(&'_') {
            cursor += 1;
        }
        let Some(octave_char) = chars.get(cursor) else {
            continue;
        };
        let Some(octave) = octave_char.to_digit(10) else {
            continue;
        };
        cursor += 1;
        if chars
            .get(cursor)
            .is_some_and(|character| character.is_ascii_alphanumeric())
        {
            continue;
        }
        let natural = match letter {
            'C' => 0,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            'B' => 11,
            _ => unreachable!(),
        };
        let midi = (octave as i16 + 1) * 12 + natural + accidental;
        if (0..=127).contains(&midi) {
            return Some(RootPitch {
                midi_note: midi as u8,
                cents: 0.0,
                confidence: 1.0,
                voiced_coverage: 1.0,
                source: PitchSource::Filename,
            });
        }
    }
    filename_midi_number(&chars)
}

fn filename_midi_number(chars: &[char]) -> Option<RootPitch> {
    for start in 0..chars.len() {
        if !chars[start].is_ascii_digit() {
            continue;
        }
        // Spec tokens look like `-060-`, not a trailing BPM (`loop-120`) or
        // an unseparated pad number (`kit-036`).
        if start == 0 || !matches!(chars[start - 1], '-' | '_') {
            continue;
        }
        let mut cursor = start;
        let mut value = 0_u32;
        let mut digits = 0_u32;
        while cursor < chars.len() && chars[cursor].is_ascii_digit() && digits < 4 {
            value = value * 10 + chars[cursor].to_digit(10).unwrap();
            digits += 1;
            cursor += 1;
        }
        if digits != 3 || value > 127 {
            continue;
        }
        if !chars
            .get(cursor)
            .is_some_and(|character| matches!(character, '-' | '_'))
        {
            continue;
        }
        return Some(RootPitch {
            midi_note: value as u8,
            cents: 0.0,
            confidence: 1.0,
            voiced_coverage: 1.0,
            source: PitchSource::Filename,
        });
    }
    None
}

pub fn detect_root_pitch(
    stereo: &[f32],
    frames: usize,
    sample_rate: f64,
    range: Range<usize>,
) -> PitchOutcome {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return PitchOutcome::NoStablePitch(NoPitchReason::TooShort);
    }
    let available = frames.min(stereo.len() / 2);
    let start = range.start.min(available);
    let end = range.end.min(available);
    let source_frames = end.saturating_sub(start);
    if source_frames as f64 / sample_rate < MIN_SECONDS {
        return PitchOutcome::NoStablePitch(NoPitchReason::TooShort);
    }

    let mono = downsample_mono(stereo, start..end, sample_rate);
    if mono.len() < FRAME_SIZE {
        return PitchOutcome::NoStablePitch(NoPitchReason::TooShort);
    }
    let rms = (mono
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>()
        / mono.len() as f64)
        .sqrt();
    if rms < SILENCE_RMS {
        return PitchOutcome::NoStablePitch(NoPitchReason::Silent);
    }

    let starts = frame_starts(mono.len());
    let total_frames = starts.len();
    let mut estimates = Vec::with_capacity(total_frames);
    let mut confidence_sum = 0.0_f64;
    for start in starts {
        if let Some((frequency, confidence)) = yin_frame(&mono[start..start + FRAME_SIZE]) {
            estimates.push(69.0 + 12.0 * (frequency / 440.0).log2());
            confidence_sum += confidence;
        }
    }
    if estimates.is_empty() {
        return PitchOutcome::NoStablePitch(NoPitchReason::Unvoiced);
    }
    let voiced_coverage = estimates.len() as f32 / total_frames.max(1) as f32;
    if voiced_coverage < MIN_VOICED_COVERAGE {
        return PitchOutcome::NoStablePitch(NoPitchReason::Unvoiced);
    }

    estimates.sort_by(f64::total_cmp);
    let center = median(&estimates);
    let mut deviations: Vec<f64> = estimates
        .iter()
        .map(|estimate| (estimate - center).abs() * 100.0)
        .collect();
    deviations.sort_by(f64::total_cmp);
    let spread = percentile(&deviations, 0.9);
    if spread > MAX_STABLE_SPREAD_CENTS {
        return PitchOutcome::NoStablePitch(NoPitchReason::Unstable);
    }
    let center_frequency = 440.0 * 2f64.powf((center - 69.0) / 12.0);
    if looks_polyphonic(&mono, center_frequency) {
        return PitchOutcome::NoStablePitch(NoPitchReason::Polyphonic);
    }
    let confidence = (confidence_sum / estimates.len() as f64) as f32;
    let midi_note = center.round().clamp(0.0, 127.0) as u8;
    let cents = ((center - f64::from(midi_note)) * 100.0) as f32;
    PitchOutcome::Detected(RootPitch {
        midi_note,
        cents,
        confidence: confidence.clamp(0.0, 1.0),
        voiced_coverage,
        source: PitchSource::Analysis,
    })
}

fn downsample_mono(stereo: &[f32], range: Range<usize>, sample_rate: f64) -> Vec<f32> {
    let source_frames = range.end - range.start;
    let capped_source_frames =
        source_frames.min((sample_rate * MAX_SECONDS).round().max(1.0) as usize);
    let output_frames =
        ((capped_source_frames as f64 * ANALYSIS_RATE / sample_rate).floor() as usize).max(1);
    let step = sample_rate / ANALYSIS_RATE;
    let mut mono = Vec::with_capacity(output_frames);
    for output in 0..output_frames {
        let source = range.start + ((output as f64 * step) as usize).min(capped_source_frames - 1);
        let left = stereo[source * 2];
        let right = stereo[source * 2 + 1];
        mono.push((left + right) * 0.5);
    }
    let mean = mono.iter().map(|sample| f64::from(*sample)).sum::<f64>() / mono.len() as f64;
    for sample in &mut mono {
        *sample -= mean as f32;
    }
    mono
}

fn frame_starts(samples: usize) -> Vec<usize> {
    let last = samples - FRAME_SIZE;
    let available = last / (FRAME_SIZE / 4) + 1;
    let count = available.clamp(1, MAX_FRAMES);
    if count == 1 {
        return vec![last / 2];
    }
    (0..count).map(|index| index * last / (count - 1)).collect()
}

fn yin_frame(frame: &[f32]) -> Option<(f64, f64)> {
    let min_tau = (ANALYSIS_RATE / MAX_FREQUENCY).floor().max(2.0) as usize;
    let max_tau = (ANALYSIS_RATE / MIN_FREQUENCY)
        .ceil()
        .min((frame.len() / 2) as f64) as usize;
    let mut difference = vec![0.0_f64; max_tau + 1];
    for tau in 1..=max_tau {
        let mut sum = 0.0_f64;
        for index in 0..frame.len() - tau {
            let delta = f64::from(frame[index]) - f64::from(frame[index + tau]);
            sum += delta * delta;
        }
        difference[tau] = sum;
    }
    let mut running = 0.0_f64;
    let mut cmndf = vec![1.0_f64; max_tau + 1];
    for tau in 1..=max_tau {
        running += difference[tau];
        if running > f64::EPSILON {
            cmndf[tau] = difference[tau] * tau as f64 / running;
        }
    }

    let mut chosen = None;
    for tau in min_tau..max_tau {
        if cmndf[tau] < YIN_THRESHOLD && cmndf[tau] <= cmndf[tau + 1] {
            chosen = Some(tau);
            break;
        }
    }
    let tau = chosen.or_else(|| {
        (min_tau..=max_tau).min_by(|left, right| cmndf[*left].total_cmp(&cmndf[*right]))
    })?;
    let confidence = 1.0 - cmndf[tau];
    if confidence < MIN_FRAME_CONFIDENCE {
        return None;
    }
    let refined = parabolic_tau(&cmndf, tau);
    Some((ANALYSIS_RATE / refined, confidence.clamp(0.0, 1.0)))
}

fn parabolic_tau(values: &[f64], index: usize) -> f64 {
    if index == 0 || index + 1 >= values.len() {
        return index as f64;
    }
    let left = values[index - 1];
    let center = values[index];
    let right = values[index + 1];
    let denominator = left - 2.0 * center + right;
    if denominator.abs() < 1.0e-12 {
        index as f64
    } else {
        index as f64 + 0.5 * (left - right) / denominator
    }
}

fn median(sorted: &[f64]) -> f64 {
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) * 0.5
    } else {
        sorted[middle]
    }
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

fn looks_polyphonic(mono: &[f32], fundamental: f64) -> bool {
    let size = FRAME_SIZE.min(mono.len());
    let start = (mono.len() - size) / 2;
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(size);
    let mut input = fft.make_input_vec();
    for (index, sample) in mono[start..start + size].iter().enumerate() {
        let window =
            0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / (size - 1).max(1) as f32).cos();
        input[index] = *sample * window;
    }
    let mut spectrum = fft.make_output_vec();
    if fft.process(&mut input, &mut spectrum).is_err() {
        return false;
    }
    let powers: Vec<f64> = spectrum
        .iter()
        .map(|bin| f64::from(bin.norm_sqr()))
        .collect();
    let peak = powers.iter().copied().fold(0.0_f64, f64::max);
    if peak <= f64::EPSILON {
        return false;
    }
    let floor = peak * 0.015;
    let bin_hz = ANALYSIS_RATE / size as f64;
    let mut total = 0.0_f64;
    let mut off_harmonic = 0.0_f64;
    for bin in 2..powers.len().saturating_sub(1) {
        let power = powers[bin];
        if power < floor || power < powers[bin - 1] || power < powers[bin + 1] {
            continue;
        }
        let frequency = bin as f64 * bin_hz;
        if !(MIN_FREQUENCY..=MAX_FREQUENCY * 4.0).contains(&frequency) {
            continue;
        }
        let harmonic = (frequency / fundamental).round();
        if harmonic < 1.0 {
            continue;
        }
        total += power;
        let expected = fundamental * harmonic;
        let cents = 1_200.0 * (frequency / expected).log2().abs();
        if cents > 55.0 {
            off_harmonic += power;
        }
    }
    total > 0.0 && off_harmonic / total > 0.22
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::io::Write;
    use std::path::Path;

    use super::{detect_root_pitch, root_hint, NoPitchReason, PitchOutcome, PitchSource};

    const RATE: f64 = 48_000.0;

    fn stereo_signal(seconds: f64, mut sample: impl FnMut(usize, f64) -> f32) -> Vec<f32> {
        let frames = (seconds * RATE) as usize;
        let mut stereo = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let value = sample(frame, frame as f64 / RATE);
            stereo.extend([value, value]);
        }
        stereo
    }

    fn sine(frequency: f64, seconds: f64) -> Vec<f32> {
        stereo_signal(seconds, |_, time| {
            (TAU * frequency * time).sin() as f32 * 0.7
        })
    }

    fn detected(stereo: &[f32]) -> (u8, f32, f32) {
        match detect_root_pitch(stereo, stereo.len() / 2, RATE, 0..stereo.len() / 2) {
            PitchOutcome::Detected(pitch) => (pitch.midi_note, pitch.cents, pitch.confidence),
            other => panic!("expected a detected pitch, got {other:?}"),
        }
    }

    #[test]
    fn detects_a4_at_concert_pitch() {
        let (note, cents, confidence) = detected(&sine(440.0, 1.0));
        assert_eq!(note, 69);
        assert!(cents.abs() < 2.0, "A4 cents error was {cents}");
        assert!(confidence >= 0.9, "A4 confidence was {confidence}");
    }

    #[test]
    fn preserves_fine_tuning_away_from_the_nearest_note() {
        let frequency = 261.625_565 * 2f64.powf(23.0 / 1_200.0);
        let (note, cents, _) = detected(&sine(frequency, 1.0));
        assert_eq!(note, 60);
        assert!((cents - 23.0).abs() < 3.0, "detected {cents} cents");
    }

    #[test]
    fn finds_a_missing_fundamental_from_its_harmonics() {
        let signal = stereo_signal(1.0, |_, time| {
            let second = (TAU * 220.0 * time).sin();
            let third = (TAU * 330.0 * time).sin() * 0.65;
            let fourth = (TAU * 440.0 * time).sin() * 0.35;
            ((second + third + fourth) * 0.35) as f32
        });
        let (note, cents, _) = detected(&signal);
        assert_eq!(note, 45, "missing fundamental should resolve to A2");
        assert!(cents.abs() < 5.0, "A2 cents error was {cents}");
    }

    #[test]
    fn aggregates_moderate_vibrato_around_its_center() {
        let mut phase = 0.0_f64;
        let signal = stereo_signal(1.5, |_, time| {
            let cents = 18.0 * (TAU * 5.0 * time).sin();
            let frequency = 329.627_557 * 2f64.powf(cents / 1_200.0);
            phase += TAU * frequency / RATE;
            phase.sin() as f32 * 0.65
        });
        let (note, cents, _) = detected(&signal);
        assert_eq!(note, 64);
        assert!(cents.abs() < 10.0, "vibrato center error was {cents}");
    }

    #[test]
    fn rejects_silence_instead_of_inventing_c() {
        let signal = vec![0.0; RATE as usize * 2];
        assert_eq!(
            detect_root_pitch(&signal, signal.len() / 2, RATE, 0..signal.len() / 2),
            PitchOutcome::NoStablePitch(NoPitchReason::Silent)
        );
    }

    #[test]
    fn rejects_a_short_transient() {
        let signal = stereo_signal(0.04, |frame, _| if frame == 0 { 1.0 } else { 0.0 });
        assert_eq!(
            detect_root_pitch(&signal, signal.len() / 2, RATE, 0..signal.len() / 2),
            PitchOutcome::NoStablePitch(NoPitchReason::TooShort)
        );
    }

    #[test]
    fn rejects_deterministic_noise_as_unvoiced() {
        let mut state = 0x1234_5678_u32;
        let signal = stereo_signal(1.0, |_, _| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((state >> 8) as f32 / 8_388_607.5 - 1.0) * 0.5
        });
        assert!(matches!(
            detect_root_pitch(&signal, signal.len() / 2, RATE, 0..signal.len() / 2),
            PitchOutcome::NoStablePitch(NoPitchReason::Unvoiced)
                | PitchOutcome::NoStablePitch(NoPitchReason::Unstable)
        ));
    }

    #[test]
    fn rejects_two_simultaneous_unrelated_notes() {
        let signal = stereo_signal(1.0, |_, time| {
            (((TAU * 261.625_565 * time).sin() + (TAU * 369.994_423 * time).sin()) * 0.35) as f32
        });
        let outcome = detect_root_pitch(&signal, signal.len() / 2, RATE, 0..signal.len() / 2);
        assert!(
            matches!(
                outcome,
                PitchOutcome::NoStablePitch(NoPitchReason::Polyphonic)
                    | PitchOutcome::NoStablePitch(NoPitchReason::Unstable)
            ),
            "inharmonic chord should be rejected, got {outcome:?}"
        );
    }

    #[test]
    fn reads_a_standalone_note_token_from_the_filename() {
        let pitch = root_hint(Path::new("Deep Bass - F#3 - long.wav")).expect("filename root");
        assert_eq!(pitch.midi_note, 54);
        assert_eq!(pitch.cents, 0.0);
        assert_eq!(pitch.source, PitchSource::Filename);
        assert_eq!(
            root_hint(Path::new("pack-060-loop.wav"))
                .expect("midi number token")
                .midi_note,
            60
        );
        assert!(root_hint(Path::new("take_2026_08.wav")).is_none());
        assert!(
            root_hint(Path::new("loop-120.wav")).is_none(),
            "BPM-like 120 must not be treated as MIDI 120"
        );
        assert!(
            root_hint(Path::new("kit-036.wav")).is_none(),
            "a trailing number without -NNN- separators is incidental"
        );
    }

    #[test]
    fn wav_sampler_metadata_precedes_a_conflicting_filename() {
        let path =
            std::env::temp_dir().join(format!("mydaw-root-hint-{}-C4.wav", std::process::id()));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&60_u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEsmpl");
        bytes.extend_from_slice(&36_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // manufacturer
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // product
        bytes.extend_from_slice(&20_833_u32.to_le_bytes()); // period
        bytes.extend_from_slice(&57_u32.to_le_bytes()); // A3
        bytes.extend_from_slice(&0x4000_0000_u32.to_le_bytes()); // +25 cents
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // SMPTE format
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // SMPTE offset
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // loop count
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // sampler data
        let mut file = std::fs::File::create(&path).expect("temp wav");
        file.write_all(&bytes).expect("write temp wav");
        drop(file);

        let pitch = root_hint(&path).expect("smpl root");
        let _ = std::fs::remove_file(&path);
        assert_eq!(pitch.midi_note, 57);
        assert!((pitch.cents - 25.0).abs() < 0.001);
        assert_eq!(pitch.source, PitchSource::Metadata);
    }
}
