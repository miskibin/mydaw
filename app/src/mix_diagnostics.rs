//! Bounded offline evidence for mixing; no perceptual quality classifier.
use mydaw_plugin_debug_tap::{analysis::spectrum, Capture};
use serde_json::{json, Value};

fn db(amplitude: f64) -> Option<f64> {
    (amplitude > 0.0).then(|| 20.0 * amplitude.log10())
}

pub fn measure(capture: &Capture) -> Value {
    let window_frames = (capture.sample_rate * 0.05).round().max(1.0) as usize;
    let mut peak = 0.0_f64;
    let mut channel_peaks = [0.0_f64; 2];
    let mut power = 0.0;
    let mut finite = 0;
    let mut non_finite = 0;
    let mut full_scale = 0;
    let mut flat_top_frames = 0;
    for (index, &sample) in capture.samples.iter().enumerate() {
        if sample.is_finite() {
            let value = f64::from(sample);
            peak = peak.max(value.abs());
            channel_peaks[index % 2] = channel_peaks[index % 2].max(value.abs());
            power += value * value;
            finite += 1;
            full_scale += usize::from(value.abs() >= 1.0);
        } else {
            non_finite += 1;
        }
    }
    let rms = (power / (finite as f64).max(1.0)).sqrt();
    let mut active_power = 0.0;
    let mut active_samples = 0;
    let mut active_windows = 0;
    let mut windows = 0;
    let mut loudest_rms = 0.0_f64;
    let mut loudest_frame = 0;
    for (index, window) in capture.samples.chunks(window_frames * 2).enumerate() {
        let mut sum = 0.0;
        let mut count = 0;
        for &sample in window {
            if sample.is_finite() {
                sum += f64::from(sample).powi(2);
                count += 1;
            }
        }
        let window_rms = (sum / (count as f64).max(1.0)).sqrt();
        windows += 1;
        if window_rms > 0.001 {
            active_windows += 1;
            active_power += sum;
            active_samples += count;
        }
        if window_rms > loudest_rms {
            loudest_rms = window_rms;
            loudest_frame = index * window_frames;
        }
    }
    if peak > 0.001 {
        for channel in 0..2 {
            let channel_peak = channel_peaks[channel];
            if channel_peak <= 0.001 {
                continue;
            }
            let mut run = 0;
            let mut last = 0.0_f32;
            for frame in capture.samples.chunks_exact(2) {
                let value = frame[channel];
                if value.is_finite()
                    && f64::from(value.abs()) >= channel_peak * 0.9
                    && f64::from((value - last).abs()) <= channel_peak * 0.000001
                {
                    run += 1;
                    if run == 2 {
                        flat_top_frames += 3;
                    } else if run > 2 {
                        flat_top_frames += 1;
                    }
                } else {
                    run = 0;
                }
                last = value;
            }
        }
    }
    let boundaries = [20.0, 150.0, 500.0, 2000.0, 6000.0, 20000.0];
    let mut bands = [0.0_f64; 5];
    let size = capture.frames().min(4096);
    let window_count = if size >= 64 {
        capture.frames().div_ceil(4096).min(64)
    } else {
        0
    };
    for index in 0..window_count {
        let start = if window_count > 1 {
            index * (capture.frames() - size) / (window_count - 1)
        } else {
            0
        };
        for channel in 0..2 {
            let samples: Vec<f32> = (start..start + size)
                .map(|frame| capture.samples[frame * 2 + channel])
                .collect();
            let fft = spectrum(&samples, capture.sample_rate);
            for (bin, &amplitude) in fft.magnitudes.iter().enumerate() {
                let hz = bin as f64 * fft.bin_hz;
                for band in 0..5 {
                    if hz >= boundaries[band] && hz < boundaries[band + 1] {
                        bands[band] += f64::from(amplitude).powi(2);
                    }
                }
            }
        }
    }
    let total: f64 = bands.iter().sum();
    let mut flags = Vec::new();
    if non_finite > 0 {
        flags.push("NON_FINITE_AUDIO");
    }
    if full_scale > 0 {
        flags.push("FULL_SCALE_RISK");
    }
    if flat_top_frames > 0 {
        flags.push("FLAT_TOP_CANDIDATE");
    }
    if peak <= 0.000001 {
        flags.push("SILENT");
    }
    json!({
        "samplePeakDbfs":db(peak), "rmsDbfs":db(rms),
        "activeRmsDbfs":db((active_power / (active_samples as f64).max(1.0)).sqrt()),
        "activeWindowFraction":active_windows as f64 / (windows as f64).max(1.0),
        "crestDb":if rms > 0.0 {db(peak / rms)} else {None},
        "maxWindowRmsDbfs":db(loudest_rms),
        "maxWindowStartSeconds":loudest_frame as f64 / capture.sample_rate,
        "overFullScaleSamples":full_scale, "nonFiniteSamples":non_finite,
        "flatTopCandidateSamples":flat_top_frames, "windowMs":50,
        "activeThresholdDbfs":-60, "spectralWindowsPerChannel":window_count,
        "bands":(0..5).map(|index|json!({"lowHz":boundaries[index],"highHz":boundaries[index+1].min(capture.sample_rate/2.0),
            "energyPercent":if total > 0.0 {Some(100.0*bands[index]/total)} else {None}})).collect::<Vec<_>>(),
        "flags":flags, "lufsIntegrated":Value::Null,"truePeakDbtp":Value::Null
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(frames: usize, f: impl Fn(usize) -> f32) -> Capture {
        Capture::new((0..frames).flat_map(|i| [f(i), f(i)]).collect(), 48000.0, 0)
    }
    #[test]
    fn silence_and_nonfinite_samples_never_produce_fake_levels() {
        let quiet = measure(&capture(4800, |_| 0.0));
        assert!(quiet["samplePeakDbfs"].is_null());
        assert!(quiet["activeRmsDbfs"].is_null());
        assert_eq!(quiet["flags"], json!(["SILENT"]));
        let bad = measure(&capture(200, |i| if i == 0 { f32::NAN } else { 0.0 }));
        assert_eq!(bad["nonFiniteSamples"], 2);
    }
    #[test]
    fn active_level_excludes_silent_gaps_and_spectrum_includes_later_sound() {
        let result = measure(&capture(48000, |i| {
            if i < 24000 {
                0.0
            } else {
                0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin()
            }
        }));
        assert!((result["samplePeakDbfs"].as_f64().unwrap() + 6.0206).abs() < 0.01);
        assert!((result["activeRmsDbfs"].as_f64().unwrap() + 9.0309).abs() < 0.02);
        assert!((result["activeWindowFraction"].as_f64().unwrap() - 0.5).abs() < 0.01);
        assert!(result["bands"][2]["energyPercent"].as_f64().unwrap() > 99.0);
        assert_eq!(result["overFullScaleSamples"], 0);
    }
    #[test]
    fn attenuation_does_not_hide_flat_top_candidates() {
        let result = measure(&capture(4800, |i| if i % 48 < 24 { 0.2 } else { -0.2 }));
        assert_eq!(result["overFullScaleSamples"], 0);
        assert!(result["flatTopCandidateSamples"].as_u64().unwrap() > 0);
        let hot = measure(&capture(4800, |_| 1.1));
        assert_eq!(hot["overFullScaleSamples"], 9600);
    }
    #[test]
    fn quiet_channel_flat_tops_are_not_hidden_by_the_loud_channel() {
        let samples = (0..4800)
            .flat_map(|i| {
                [
                    (std::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin() * 0.8,
                    if i % 48 < 24 { 0.05 } else { -0.05 },
                ]
            })
            .collect();
        let result = measure(&Capture::new(samples, 48000.0, 0));
        assert!(result["flatTopCandidateSamples"].as_u64().unwrap() > 4000);
    }
    #[test]
    fn ordinary_quiet_bass_crests_are_not_flat_top_candidates() {
        let result = measure(&capture(48000, |i| {
            (std::f32::consts::TAU * 40.0 * i as f32 / 48000.0).sin() * 0.01
        }));
        assert_eq!(result["flatTopCandidateSamples"], 0);
    }
}
