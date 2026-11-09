//! Measurements of the character saturator (docs/saturator.md):
//! `cargo run -p wave-engine --release --example sat_measure`
//!
//! THD and harmonics per voicing, aliasing of a 5 kHz tone at full drive, auto-gain
//! level match on pink noise, latency per mode and CPU per instance.

use std::time::Instant;

use realfft::RealFftPlanner;
use wave_engine::dsp::saturator::{
    latency_exact, latency_samples, pink_noise, Core, Settings, STYLES,
};

const SR: f32 = 48000.0;

fn run(set: Settings, x: &[f32], os: Option<usize>, adaa: bool) -> Vec<f32> {
    let mut c = Core::default();
    c.measurement(os, adaa);
    c.set(&set);
    c.prepare(SR, 1);
    c.set(&set);
    let mut y = x.to_vec();
    for ch in y.chunks_mut(512) {
        c.process_mono(ch);
    }
    y
}

/// Magnitude spectrum of exactly 48000 samples (1 Hz bins, rectangular window: test
/// tones are whole periods, so every harmonic falls on a bin).
fn spectrum(y: &[f32]) -> Vec<f32> {
    let n = y.len();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut inp = y.to_vec();
    let mut out = fft.make_output_vec();
    fft.process(&mut inp, &mut out).unwrap();
    out.iter().map(|c| c.norm() * 2.0 / n as f32).collect()
}

fn sine(f: f32, amp: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (std::f64::consts::TAU * f as f64 * i as f64 / SR as f64).sin() as f32 * amp)
        .collect()
}

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-12).log10()
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt()
}

fn main() {
    // ---------------------------------------------------------------- THD
    println!("## THD, 1 kHz sine at -12 dBFS peak, 2x, auto gain on\n");
    println!("| voicing | drive | THD % | H2 dBc | H3 dBc | H4 dBc | H5 dBc |");
    println!("|---|---|---|---|---|---|---|");
    let x = sine(1000.0, 0.25, 96000);
    for (s, name) in STYLES.iter().enumerate() {
        for d in [0.25f32, 0.5, 1.0] {
            let y = run(
                Settings {
                    style: s,
                    drive: d,
                    ..Default::default()
                },
                &x,
                None,
                true,
            );
            let sp = spectrum(&y[48000..96000]);
            let h = |k: usize| sp[1000 * k];
            let thd = (2..=20).map(|k| h(k).powi(2)).sum::<f32>().sqrt() / h(1);
            println!(
                "| {name} | {:.0} % | {:.2} | {:.1} | {:.1} | {:.1} | {:.1} |",
                d * 100.0,
                thd * 100.0,
                db(h(2) / h(1)),
                db(h(3) / h(1)),
                db(h(4) / h(1)),
                db(h(5) / h(1))
            );
        }
    }

    // ---------------------------------------------------------------- aliasing
    println!("\n## Aliasing, 5 kHz sine at -6 dBFS peak, drive 100 %, 48 kHz\n");
    println!("Energy off the harmonic bins (everything except DC and k x 5 kHz), relative to the output, dB.\n");
    println!("| voicing | 1x naive | 1x ADAA | 2x + ADAA (normal) | 4x + ADAA (HQ) |");
    println!("|---|---|---|---|---|");
    let x = sine(5000.0, 0.5, 96000);
    for (s, name) in STYLES.iter().enumerate() {
        let mut row = format!("| {name} |");
        for (os, adaa) in [
            (Some(1), false),
            (Some(1), true),
            (None, true),
            (Some(4), true),
        ] {
            let set = Settings {
                style: s,
                drive: 1.0,
                hq: os == Some(4),
                ..Default::default()
            };
            let y = run(set, &x, os, adaa);
            let sp = spectrum(&y[48000..96000]);
            let (mut on, mut off) = (0f64, 0f64);
            for (f, &m) in sp.iter().enumerate().take(24000).skip(20) {
                let e = (m as f64).powi(2);
                let near = (1..=4).any(|k| (f as i64 - 5000 * k).abs() <= 2);
                if near {
                    on += e
                } else {
                    off += e
                }
            }
            row += &format!(" {:.1} |", 10.0 * (off / (on + off)).log10());
        }
        println!("{row}");
    }

    // ---------------------------------------------------------------- auto gain
    println!("\n## Auto gain: output minus input level, pink noise, dB\n");
    for level in [-18.0f32, -12.0, -24.0] {
        println!("Input {level} dBFS RMS:\n");
        println!("| voicing | 0 % | 10 | 20 | 30 | 40 | 50 | 60 | 70 | 80 | 90 | 100 % | 100 % + Punish | max abs |");
        println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
        let x = pink_noise(96000, level, 101);
        let rin = rms(&x[24000..]);
        for (s, name) in STYLES.iter().enumerate() {
            let mut row = format!("| {name} |");
            let mut worst = 0f32;
            for d in 0..=11 {
                let set = if d == 11 {
                    Settings {
                        style: s,
                        drive: 1.0,
                        punish: true,
                        ..Default::default()
                    }
                } else {
                    Settings {
                        style: s,
                        drive: d as f32 / 10.0,
                        ..Default::default()
                    }
                };
                let y = run(set, &x, None, true);
                let v = db(rms(&y[24000..]) / rin);
                if d < 11 {
                    worst = worst.max(v.abs());
                }
                row += &format!(" {v:+.2} |");
            }
            println!("{row} {worst:.2} |");
        }
        println!();
    }

    // ---------------------------------------------------------------- latency
    println!("## Latency\n");
    for (hq, name) in [(false, "2x (normal)"), (true, "4x (HQ)")] {
        println!("- {name}: {:.2} samples group delay near DC at the base rate, reported {} ({:.3} ms at 48 kHz)", latency_exact(hq), latency_samples(hq), latency_samples(hq) as f32 / 48.0);
    }

    // ---------------------------------------------------------------- CPU
    println!("\n## CPU per stereo instance, 48 kHz, blocks of 256\n");
    println!("| voicing | 2x ns/sample | 2x % of one core | 4x ns/sample | 4x % of one core |");
    println!("|---|---|---|---|---|");
    let secs = 10usize;
    let noise = pink_noise(48000 * secs, -18.0, 5);
    for (s, name) in STYLES.iter().enumerate() {
        let mut row = format!("| {name} |");
        for hq in [false, true] {
            let set = Settings {
                style: s,
                drive: 0.6,
                hq,
                lowcut: 60.0,
                tone: 0.2,
                highcut: 12000.0,
                ..Default::default()
            };
            let mut c = Core::default();
            c.set(&set);
            c.prepare(SR, 2);
            c.set(&set);
            let (mut l, mut r) = (noise.clone(), noise.clone());
            let t = Instant::now();
            for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
                c.process(a, Some(b));
            }
            let el = t.elapsed().as_secs_f64();
            let ns = el * 1e9 / (48000 * secs) as f64;
            row += &format!(" {ns:.0} | {:.2} % |", el / secs as f64 * 100.0);
            std::hint::black_box(&l);
        }
        println!("{row}");
    }
}
