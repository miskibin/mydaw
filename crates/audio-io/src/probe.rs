//! Round-trip calibration: play a few clicks, record them back (mic held to the speaker or
//! a loopback cable), find each click in the recording by cross-correlation.
//!
//! The output callback plays [`ClickPlan::signal`] instead of the mix and stores, for every
//! output frame it renders, the input frame it consumed alongside it ([`Probe::cap`]). A
//! click rendered at output frame `a` that comes back in `cap` at `a + lag` means: input
//! consumed with output frame `j + lag` was captured when frame `j` was heard. That `lag`
//! is exactly what a take has to drop from its front, converters, USB and driver buffers
//! included, so `lag − reported` is the calibration offset.

use serde::Serialize;

/// Result of [`crate::AudioIo::measure_latency`].
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LatencyReport {
    /// Measured round trip: a sound played reaches the recording this much later.
    pub round_trip_ms: f32,
    /// What the driver timestamps say (the alignment takes use without an offset).
    pub reported_ms: f32,
    /// `round_trip_ms − reported_ms`: the `recOffsetMs` that makes takes land on the beat.
    pub suggested_offset_ms: f32,
    /// 0..1: share of clicks that agree within 1 ms × how clearly the peak stands out from
    /// echoes and noise. Below ~0.5 the measurement should be repeated.
    pub confidence: f32,
}

/// A measurement job handed to the output callback (allocated here, never on the audio
/// thread; it travels there and back through lock-free rings).
pub(crate) struct Probe {
    /// Tells a measurement's result from an abandoned earlier one.
    pub id: u64,
    pub signal: Vec<f32>,
    pub cap: Vec<f32>,
    pub pos: usize,
    /// Driver-reported round trips (frames, no offset) seen while the probe ran.
    pub reported_sum: f64,
    pub reported_n: u32,
}

impl Probe {
    pub fn new(id: u64, signal: Vec<f32>) -> Self {
        let cap = vec![0.0; signal.len()];
        Self {
            id,
            signal,
            cap,
            pos: 0,
            reported_sum: 0.0,
            reported_n: 0,
        }
    }
    pub fn done(&self) -> bool {
        self.pos >= self.signal.len()
    }
}

pub struct ClickPlan {
    /// What the output plays (mono, both channels).
    pub signal: Vec<f32>,
    /// One click: a short windowed noise burst (a sharp, unambiguous correlation peak).
    pub burst: Vec<f32>,
    /// Frame where each click starts in `signal`.
    pub at: Vec<usize>,
    /// Longest round trip searched for.
    pub max_lag: usize,
}

const CLICKS: usize = 8;

/// Eight clicks after 0.6 s of silence (the input settles), irregularly spaced so a later
/// click is never mistaken for a long round trip of an earlier one. Round trips up to
/// 400 ms are found (Bluetooth and heavily buffered devices included).
pub fn click_plan(rate: u32) -> ClickPlan {
    let ms = |x: f64| (x * rate as f64 / 1000.0).round() as usize;
    let n = ms(4.0).max(16);
    let mut seed = 0x9e37_79b9u32;
    let burst: Vec<f32> = (0..n)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / (n - 1) as f64).cos();
            ((seed as f64 / u32::MAX as f64) * 2.0 - 1.0) as f32 * w as f32 * 0.5
        })
        .collect();
    let max_lag = ms(400.0);
    let mut at = vec![];
    let mut t = ms(600.0);
    for k in 0..CLICKS {
        at.push(t);
        t += max_lag + n + ms(60.0 + 23.0 * k as f64);
    }
    let len = t + max_lag;
    let mut signal = vec![0.0; len];
    for &a in &at {
        signal[a..a + n].copy_from_slice(&burst);
    }
    ClickPlan {
        signal,
        burst,
        at,
        max_lag,
    }
}

/// Normalised cross-correlation of the clicks with `cap` at `lag`, over the given clicks.
fn ncc(plan: &ClickPlan, cap: &[f32], clicks: &[usize], lag: usize, b_energy: f64) -> f64 {
    let n = plan.burst.len();
    let (mut num, mut en) = (0.0f64, 0.0f64);
    for &a in clicks {
        let Some(x) = cap.get(a + lag..a + lag + n) else {
            continue;
        };
        for (s, b) in x.iter().zip(&plan.burst) {
            num += *s as f64 * *b as f64;
            en += *s as f64 * *s as f64;
        }
    }
    let den = (en * b_energy * clicks.len() as f64).sqrt();
    if den <= 1e-12 {
        0.0
    } else {
        (num / den).abs()
    }
}

/// The round trip (frames) and a confidence in 0..1, or `None` when no click came back.
pub fn find_lag(plan: &ClickPlan, cap: &[f32], rate: u32) -> Option<(usize, f32)> {
    let b_energy: f64 = plan.burst.iter().map(|x| *x as f64 * *x as f64).sum();
    let all: Vec<usize> = plan.at.clone();
    let curve: Vec<f64> = (0..plan.max_lag)
        .map(|l| ncc(plan, cap, &all, l, b_energy))
        .collect();
    let (lag, peak) = curve
        .iter()
        .copied()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if peak < 0.1 {
        return None;
    }
    let ms1 = (rate as usize / 1000).max(1);
    // how clearly the peak stands out from everything more than 2 ms away (echoes, noise)
    let second = curve
        .iter()
        .enumerate()
        .filter(|(l, _)| l.abs_diff(lag) > 2 * ms1)
        .map(|(_, v)| *v)
        .fold(0.0, f64::max);
    // (a room echo at half the direct sound is normal and costs nothing)
    let prominence = (2.0 * (1.0 - second / peak)).clamp(0.0, 1.0);
    // each click on its own: does it agree within 1 ms?
    let agree = plan
        .at
        .iter()
        .filter(|&&a| {
            let one = [a];
            let best = (0..plan.max_lag).max_by(|&x, &y| {
                ncc(plan, cap, &one, x, b_energy).total_cmp(&ncc(plan, cap, &one, y, b_energy))
            });
            best.is_some_and(|b| b.abs_diff(lag) <= ms1)
        })
        .count() as f64
        / plan.at.len() as f64;
    Some((lag, (agree * prominence) as f32))
}

/// The report for a finished probe.
pub fn report(plan: &ClickPlan, probe: &Probe, rate: u32) -> Result<LatencyReport, String> {
    let (lag, confidence) = find_lag(plan, &probe.cap, rate).ok_or("No click came back. Hold the microphone close to the speaker (or connect an output to an input with a cable), raise the input gain and try again.")?;
    let ms = |f: f64| (f * 1000.0 / rate as f64) as f32;
    let round_trip_ms = ms(lag as f64);
    let reported_ms = if probe.reported_n > 0 {
        ms(probe.reported_sum / probe.reported_n as f64)
    } else {
        0.0
    };
    Ok(LatencyReport {
        round_trip_ms,
        reported_ms,
        suggested_offset_ms: round_trip_ms - reported_ms,
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loopback with a known delay, background noise and a quieter, inverted echo.
    fn loopback(plan: &ClickPlan, delay: usize, noise: f32, gain: f32) -> Vec<f32> {
        let mut seed = 12345u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        let n = plan.signal.len();
        (0..n)
            .map(|i| {
                let d = if i >= delay {
                    plan.signal[i - delay] * gain
                } else {
                    0.0
                };
                let echo = if i >= delay + 700 {
                    -plan.signal[i - delay - 700] * gain * 0.4
                } else {
                    0.0
                };
                d + echo + noise * rnd()
            })
            .collect()
    }

    #[test]
    fn finds_a_known_round_trip_in_a_noisy_loopback() {
        let rate = 48000;
        let plan = click_plan(rate);
        for delay in [37usize, 1234, 4321] {
            let cap = loopback(&plan, delay, 0.05, 0.3);
            let (lag, conf) = find_lag(&plan, &cap, rate).expect("found the clicks");
            assert_eq!(lag, delay, "lag {lag} instead of {delay}");
            assert!(conf > 0.5, "confidence {conf}");
        }
    }

    #[test]
    fn report_suggests_the_difference_to_what_the_driver_said() {
        let rate = 48000;
        let plan = click_plan(rate);
        let mut p = Probe::new(1, plan.signal.clone());
        p.cap = loopback(&plan, 1440, 0.02, 0.5); // 30 ms
        p.pos = p.signal.len();
        p.reported_sum = 1200.0 * 10.0; // the driver says 25 ms
        p.reported_n = 10;
        let r = report(&plan, &p, rate).unwrap();
        assert!((r.round_trip_ms - 30.0).abs() < 0.03, "{r:?}");
        assert!((r.reported_ms - 25.0).abs() < 0.03, "{r:?}");
        assert!((r.suggested_offset_ms - 5.0).abs() < 0.05, "{r:?}");
    }

    #[test]
    fn silence_is_not_a_measurement() {
        let rate = 44100;
        let plan = click_plan(rate);
        let mut seed = 7u32;
        let cap: Vec<f32> = (0..plan.signal.len())
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.01
            })
            .collect();
        let got = find_lag(&plan, &cap, rate);
        assert!(got.is_none_or(|(_, c)| c < 0.3), "{got:?}");
    }
}
