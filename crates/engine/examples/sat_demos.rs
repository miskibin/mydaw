//! Listening demos for the character saturator (docs/saturator.md):
//! `cargo run -p wave-engine --release --example sat_demos -- <vocal.wav> <out dir>`
//!
//! Writes WAVs (44.1 kHz): the vocal dry then each voicing; a synthesized drum loop the
//! same way; a few `sat` presets on the vocal; and Voice's drive, the previous curve
//! against the new one. Every segment is RMS-matched to the dry one (so only the
//! character differs), 1 s of silence between segments.

use std::path::{Path, PathBuf};

use wave_engine::catalog::{load_preset, new_fx, param_block, presets};
use wave_engine::dsp::create;
use wave_engine::dsp::saturator::{Core, Settings, STYLES};
use wave_engine::{FxKind, ProcessCtx};

const SR: f32 = 44100.0;

fn read_mono(p: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(p).expect("vocal wav");
    let spec = r.spec();
    let ch = spec.channels as usize;
    let v: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r.samples::<i32>().map(|s| s.unwrap() as f32 / (1u32 << (spec.bits_per_sample - 1)) as f32).collect(),
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
    };
    assert_eq!(spec.sample_rate, SR as u32, "demo expects 44.1 kHz");
    v.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn write(path: &Path, segs: &[Vec<f32>]) {
    let spec = hound::WavSpec { channels: 1, sample_rate: SR as u32, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let r0 = rms(&segs[0]);
    // one common scale so the loudest segment peaks at -1 dBFS (no clipping, same RMS)
    let peak = segs.iter().map(|s| s.iter().fold(0f32, |m, v| m.max(v.abs())) * r0 / rms(s).max(1e-9)).fold(0f32, f32::max);
    let k = (0.891 / peak.max(1e-9)).min(1.0);
    for (i, s) in segs.iter().enumerate() {
        if i > 0 {
            for _ in 0..SR as usize {
                w.write_sample(0i16).unwrap();
            }
        }
        let g = k * r0 / rms(s).max(1e-9);
        for &v in s {
            w.write_sample(((v * g).clamp(-1.0, 1.0) * 32767.0) as i16).unwrap();
        }
    }
    w.finalize().unwrap();
    println!("wrote {}", path.display());
}

fn sat(x: &[f32], set: Settings) -> Vec<f32> {
    let mut c = Core::default();
    c.set(&set);
    c.prepare(SR, 1);
    c.set(&set);
    let lat = c.latency();
    let mut y = x.to_vec();
    y.extend(std::iter::repeat_n(0.0, lat));
    for ch in y.chunks_mut(256) {
        c.process_mono(ch);
    }
    y.drain(..lat);
    y
}

fn run_fx(fx: &wave_engine::Fx, x: &[f32]) -> Vec<f32> {
    let mut p = create(fx.kind).unwrap();
    p.prepare(SR, 256);
    p.set_params(&param_block(fx));
    let lat = wave_engine::catalog::latency(fx, SR);
    let mut l = x.to_vec();
    l.extend(std::iter::repeat_n(0.0, lat));
    let mut r = l.clone();
    let ctx = ProcessCtx { sample_rate: SR, ..Default::default() };
    for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        p.process(a, b, &ctx);
    }
    l.drain(..lat);
    l
}

/// Two bars of kick / snare / hats at 92 BPM, twice.
fn drums() -> Vec<f32> {
    let beat = 60.0 / 92.0;
    let n = (beat * 16.0 * SR) as usize;
    let mut y = vec![0f32; n];
    let mut seed = 12345u32;
    let noise = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    };
    let mut add = |at: f32, f: &mut dyn FnMut(f32) -> f32, len: f32| {
        let s0 = (at * SR) as usize;
        for i in 0..(len * SR) as usize {
            if s0 + i < y.len() {
                y[s0 + i] += f(i as f32 / SR);
            }
        }
    };
    for bar in 0..4 {
        let t0 = bar as f32 * 4.0 * beat;
        for k in [0.0, 1.5, 2.0 + if bar % 2 == 1 { 0.75 } else { 0.0 }] {
            let mut ph = 0f32;
            add(
                t0 + k * beat,
                &mut |t| {
                    let f = 45.0 + 110.0 * (-t * 30.0).exp();
                    ph += std::f32::consts::TAU * f / SR;
                    ph.sin() * (-t * 6.0).exp() * 0.8
                },
                0.6,
            );
        }
        for k in [1.0, 3.0] {
            let mut lp = 0f32;
            let mut ph = 0f32;
            let mut nz = noise;
            add(
                t0 + k * beat,
                &mut |t| {
                    lp += (nz() - lp) * 0.5;
                    ph += std::f32::consts::TAU * 185.0 / SR;
                    (lp * (-t * 18.0).exp() * 0.6) + ph.sin() * (-t * 30.0).exp() * 0.35
                },
                0.35,
            );
        }
        for k in 0..8 {
            let mut prev = 0f32;
            let mut nz = noise;
            add(
                t0 + k as f32 * 0.5 * beat,
                &mut |t| {
                    let w = nz();
                    let hp = w - prev;
                    prev = w;
                    hp * (-t * 70.0).exp() * if k % 2 == 0 { 0.18 } else { 0.1 }
                },
                0.08,
            );
        }
    }
    y
}

/// Voice's drive before the character saturator: asymmetric tanh, 10 Hz DC block.
fn old_voice_drive(x: &[f32], drive_db: f32) -> Vec<f32> {
    const BIAS: f32 = 0.25;
    let g = 10f32.powf(drive_db / 20.0);
    let norm = 1.0 - BIAS.tanh().powi(2);
    let k = 1.0 - std::f32::consts::TAU * 10.0 / SR;
    let (mut px, mut py) = (0f32, 0f32);
    x.iter()
        .map(|&y| {
            let s = ((y * g + BIAS).tanh() - BIAS.tanh()) / (g.sqrt() * norm);
            let hp = s - px + k * py;
            px = s;
            py = hp;
            hp
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let vocal = read_mono(Path::new(&args[1]));
    let out = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out).unwrap();

    let mut segs = vec![vocal.clone()];
    for s in 0..STYLES.len() {
        segs.push(sat(&vocal, Settings { style: s, drive: 0.55, lowcut: 60.0, ..Default::default() }));
    }
    write(&out.join("01_vocal_styles.wav"), &segs);

    let d = drums();
    let mut segs = vec![d.clone()];
    for s in 0..STYLES.len() {
        segs.push(sat(&d, Settings { style: s, drive: 0.65, ..Default::default() }));
    }
    write(&out.join("02_drums_styles.wav"), &segs);

    let names: Vec<&str> = presets(FxKind::Sat).iter().map(|p| p.name).collect();
    let mut segs = vec![vocal.clone()];
    for want in ["Wokal — ciepło", "Wokal — lampa", "Rap — ziarno", "Lo-fi głośnik", "Równoległe zgniecenie"] {
        let i = names.iter().position(|n| *n == want).unwrap();
        let mut fx = new_fx(1, FxKind::Sat, 92.0);
        load_preset(&mut fx, i, 92.0);
        segs.push(run_fx(&fx, &vocal));
    }
    write(&out.join("03_vocal_presets.wav"), &segs);
    let mut segs = vec![d.clone()];
    for want in ["Bębny — smash", "Równoległe zgniecenie", "Bas — waga"] {
        let i = names.iter().position(|n| *n == want).unwrap();
        let mut fx = new_fx(1, FxKind::Sat, 92.0);
        load_preset(&mut fx, i, 92.0);
        segs.push(run_fx(&fx, &d));
    }
    write(&out.join("04_drums_presets.wav"), &segs);

    // Voice: drive 0 (pitch/formant path only), previous drive, new drive, both at 12 dB
    let mut voice = new_fx(1, FxKind::Voice, 92.0);
    let clean = run_fx(&voice, &vocal);
    voice.params.insert("drive".into(), 12.0);
    let new = run_fx(&voice, &vocal);
    write(&out.join("05_voice_drive_old_vs_new.wav"), &[clean.clone(), old_voice_drive(&clean, 12.0), new]);
    voice.params.insert("drive".into(), 24.0);
    let new24 = run_fx(&voice, &vocal);
    write(&out.join("06_voice_drive24_old_vs_new.wav"), &[clean.clone(), old_voice_drive(&clean, 24.0), new24]);
}
