//! Pitch-preserving time-stretch of a whole buffer, offline.
//!
//! Wraps [Signalsmith Stretch](https://github.com/Signalsmith-Audio/signalsmith-stretch)
//! (MIT, vendored in `vendor/`). Never call this from the audio callback: it allocates
//! and does work proportional to the buffer.

use std::os::raw::c_int;

extern "C" {
    fn wave_stretch_exact(
        sample_rate: f32,
        channels: c_int,
        input: *const *const f32,
        in_len: usize,
        output: *const *mut f32,
        out_len: usize,
        semitones: f32,
    ) -> c_int;
    fn wave_stretch_min_output(sample_rate: f32, channels: c_int) -> usize;
}

/// Shortest and longest supported ratio (output length / input length).
pub const MIN_RATIO: f64 = 0.25;
pub const MAX_RATIO: f64 = 4.0;

/// Output length in frames for `frames` stretched by `ratio`.
pub fn out_frames(frames: usize, ratio: f64) -> usize {
    ((frames as f64) * ratio).round().max(0.0) as usize
}

/// Stretches interleaved stereo `input` at `sr` to `ratio` times its length, keeping
/// pitch. The result has exactly [`out_frames`] frames.
pub fn stretch_stereo(input: &[f32], sr: u32, ratio: f64) -> Result<Vec<f32>, String> {
    stretch_shift_stereo(input, sr, ratio, 0.0)
}

/// Farthest supported transposition, in semitones either way.
pub const MAX_SEMITONES: f32 = 24.0;

/// [`stretch_stereo`] that also transposes by `semitones` (length unchanged by it), in
/// the same pass.
pub fn stretch_shift_stereo(input: &[f32], sr: u32, ratio: f64, semitones: f32) -> Result<Vec<f32>, String> {
    if !(semitones.is_finite() && semitones.abs() <= MAX_SEMITONES + 1e-4) {
        return Err(format!("transposition {semitones} is outside ±{MAX_SEMITONES} semitones"));
    }
    if !(ratio.is_finite() && (MIN_RATIO - 1e-9..=MAX_RATIO + 1e-9).contains(&ratio)) {
        return Err(format!("stretch ratio {ratio} is outside {MIN_RATIO}–{MAX_RATIO}"));
    }
    let frames = input.len() / 2;
    let out_len = out_frames(frames, ratio);
    if frames == 0 || out_len == 0 {
        return Ok(vec![0.0; out_len * 2]);
    }
    if (ratio - 1.0).abs() < 1e-9 && semitones == 0.0 {
        return Ok(input[..frames * 2].to_vec());
    }
    let srf = sr.max(1) as f32;
    // very short buffers: pad with silence so the algorithm has room, then crop
    let min_out = unsafe { wave_stretch_min_output(srf, 2) } + 1;
    let (in_n, out_n) = if out_len < min_out { (((min_out as f64) / ratio).ceil() as usize, min_out) } else { (frames, out_len) };
    let mut l = vec![0.0f32; in_n];
    let mut r = vec![0.0f32; in_n];
    for (i, [a, b]) in input.as_chunks::<2>().0.iter().enumerate() {
        l[i] = *a;
        r[i] = *b;
    }
    let mut ol = vec![0.0f32; out_n];
    let mut or = vec![0.0f32; out_n];
    let ins = [l.as_ptr(), r.as_ptr()];
    let outs = [ol.as_mut_ptr(), or.as_mut_ptr()];
    let rc = unsafe { wave_stretch_exact(srf, 2, ins.as_ptr(), in_n, outs.as_ptr(), out_n, semitones) };
    match rc {
        0 => {}
        1 => return Err("audio too short to stretch".into()),
        _ => return Err("not enough memory to stretch".into()),
    }
    let mut out = Vec::with_capacity(out_len * 2);
    for i in 0..out_len {
        let (a, b) = (ol[i], or[i]);
        out.push(if a.is_finite() { a } else { 0.0 });
        out.push(if b.is_finite() { b } else { 0.0 });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, sr: u32, secs: f64) -> Vec<f32> {
        let n = (sr as f64 * secs) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (0.5 * (2.0 * std::f64::consts::PI * freq * i as f64 / sr as f64).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    /// Frequency from upward zero crossings of the left channel in the middle half.
    fn freq_of(x: &[f32], sr: u32) -> f64 {
        let l: Vec<f32> = x.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
        let (a, b) = (l.len() / 4, l.len() * 3 / 4);
        let mut cross = vec![];
        for i in a..b {
            if l[i - 1] < 0.0 && l[i] >= 0.0 {
                let t = (i - 1) as f64 + (-l[i - 1] / (l[i] - l[i - 1])) as f64;
                cross.push(t);
            }
        }
        (cross.len() - 1) as f64 / ((cross[cross.len() - 1] - cross[0]) / sr as f64)
    }

    #[test]
    fn exact_length_and_pitch_kept() {
        let sr = 48_000;
        let x = sine(440.0, sr, 2.0);
        for ratio in [0.5, 0.8, 1.5, 2.0] {
            let y = stretch_stereo(&x, sr, ratio).unwrap();
            assert_eq!(y.len() / 2, out_frames(x.len() / 2, ratio), "ratio {ratio}");
            let f = freq_of(&y, sr);
            assert!((f - 440.0).abs() < 4.4, "ratio {ratio}: {f} Hz");
            // steady-state level (a 0.5 sine has an RMS of 0.354)
            let mid = &y[y.len() / 4..y.len() * 3 / 4];
            let rms = (mid.iter().map(|v| v * v).sum::<f32>() / mid.len() as f32).sqrt();
            assert!((rms - 0.354).abs() < 0.04, "ratio {ratio}: rms {rms}");
            let peak = y.iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!(peak < 1.0, "ratio {ratio}: peak {peak}");
        }
    }

    #[test]
    fn short_and_empty_buffers() {
        let sr = 44_100;
        let x = sine(1000.0, sr, 0.02);
        let y = stretch_stereo(&x, sr, 0.5).unwrap();
        assert_eq!(y.len() / 2, out_frames(x.len() / 2, 0.5));
        assert!(y.iter().all(|v| v.is_finite()));
        assert!(stretch_stereo(&[], sr, 2.0).unwrap().is_empty());
        assert!(stretch_stereo(&x, sr, 10.0).is_err());
        assert!(stretch_stereo(&x, sr, f64::NAN).is_err());
    }

    /// `cargo test -p wave-stretch --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn speed() {
        let sr = 44_100;
        let x = sine(330.0, sr, 180.0);
        for ratio in [0.5, 1.5] {
            let t = std::time::Instant::now();
            stretch_stereo(&x, sr, ratio).unwrap();
            println!("3 min stereo ×{ratio}: {:?}", t.elapsed());
        }
    }

    #[test]
    fn transposes_and_keeps_the_length() {
        let sr = 48_000;
        let x = sine(440.0, sr, 1.0);
        for (ratio, semis) in [(1.0, 7.0f32), (1.0, -12.0), (1.0, 12.0), (2.0, 3.0), (0.5, -5.5)] {
            let y = stretch_shift_stereo(&x, sr, ratio, semis).unwrap();
            assert_eq!(y.len() / 2, out_frames(x.len() / 2, ratio), "×{ratio} {semis} st");
            let want = 440.0 * 2f64.powf(semis as f64 / 12.0);
            let f = freq_of(&y, sr);
            // within 10 cents
            assert!((1200.0 * (f / want).log2()).abs() < 10.0, "×{ratio} {semis} st: {f} Hz, want {want}");
            let mid = &y[y.len() / 4..y.len() * 3 / 4];
            let rms = (mid.iter().map(|v| v * v).sum::<f32>() / mid.len() as f32).sqrt();
            assert!((rms - 0.354).abs() < 0.06, "×{ratio} {semis} st: rms {rms}");
        }
        assert!(stretch_shift_stereo(&x, sr, 1.0, 30.0).is_err());
        assert!(stretch_shift_stereo(&x, sr, 1.0, f32::NAN).is_err());
    }

    #[test]
    fn unity_is_identity() {
        let x = sine(220.0, 44_100, 0.3);
        assert_eq!(stretch_stereo(&x, 44_100, 1.0).unwrap(), x);
    }
}
