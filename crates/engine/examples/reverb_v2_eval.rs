//! Objective comparison of the old and new built-in reverb and limiter, plus listening renders.
//!
//! `cargo run -p wave-engine --example reverb_v2_eval --release -- <samples dir>`
//! The samples dir must contain `src/vocal_dry.wav` and `src/mix*.wav` (mono, any rate);
//! renders go to `<dir>/*.wav`. Without a dir only the synthetic measurements run.

use std::time::Instant;

use realfft::RealFftPlanner;
use wave_engine::catalog::{load_preset, new_fx, param_block, presets};
use wave_engine::dsp::limiter_v2::LimiterV2;
use wave_engine::dsp::reverb_v2::{self, ReverbV2};
use wave_engine::dsp::{Biquad, FilterType, Limiter as OldLimiter, Reverb as OldReverb};
use wave_engine::processor::{ParamBlock, ProcessCtx, Processor};
use wave_engine::FxKind;

const BLOCK: usize = 128;

fn run(p: &mut dyn Processor, l: &[f32], r: &[f32], sr: f32) -> (Vec<f32>, Vec<f32>) {
    let ctx = ProcessCtx {
        sample_rate: sr,
        ..Default::default()
    };
    let (mut ol, mut or) = (l.to_vec(), r.to_vec());
    for (a, b) in ol.chunks_mut(BLOCK).zip(or.chunks_mut(BLOCK)) {
        p.process(a, b, &ctx);
    }
    (ol, or)
}

fn block(vals: &[f32]) -> ParamBlock {
    let mut pb = ParamBlock::default();
    pb.values[..vals.len()].copy_from_slice(vals);
    pb
}

fn preset_block(kind: FxKind, name: &str) -> ParamBlock {
    let mut fx = new_fx(1, kind, 120.0);
    let i = presets(kind)
        .iter()
        .position(|p| p.name == name)
        .expect("preset");
    load_preset(&mut fx, i, 120.0);
    param_block(&fx)
}

/// Wet-only impulse response (L, R).
fn ir_new(pb: &ParamBlock, sr: f32, secs: f32) -> (Vec<f32>, Vec<f32>) {
    let mut v = *pb;
    v.values[reverb_v2::idx::MIX] = 100.0;
    v.values[reverb_v2::idx::DUCK] = 0.0;
    let mut p = ReverbV2::default();
    p.prepare(sr, BLOCK);
    p.set_params(&v);
    let n = (sr * secs) as usize;
    let mut x = vec![0f32; n];
    x[0] = 1.0;
    run(&mut p, &x, &x, sr)
}

/// Old reverb: values = size, pre, lo, hi, width, mix; mix 100 % still leaves half the dry in.
fn ir_old(size: f32, pre: f32, lo: f32, hi: f32, sr: f32, secs: f32) -> (Vec<f32>, Vec<f32>) {
    let mut p = OldReverb::default();
    p.prepare(sr, BLOCK);
    p.set_params(&block(&[size, pre, lo, hi, 100.0, 100.0]));
    let n = (sr * secs) as usize;
    let mut x = vec![0f32; n];
    x[0] = 1.0;
    let (mut l, mut r) = run(&mut p, &x, &x, sr);
    l[0] -= 0.5;
    r[0] -= 0.5;
    (l, r)
}

fn bandpass(x: &[f32], f: f32, sr: f32) -> Vec<f32> {
    let mut a = Biquad::default();
    let mut b = Biquad::default();
    a.set(FilterType::Bandpass, sr, f, 1.414, 0.0);
    b.set(FilterType::Bandpass, sr, f, 1.414, 0.0);
    x.iter().map(|&s| b.tick(a.tick(s))).collect()
}

/// RT60 from Schroeder backward integration, line fit −5…−35 dB (falls back to −25 dB).
fn rt60(h: &[f32], sr: f32) -> f32 {
    let mut edc = vec![0f64; h.len()];
    let mut acc = 0f64;
    for i in (0..h.len()).rev() {
        acc += (h[i] as f64).powi(2);
        edc[i] = acc;
    }
    let e0 = edc[0].max(1e-30);
    let db: Vec<f64> = edc
        .iter()
        .map(|e| 10.0 * (e / e0).max(1e-30).log10())
        .collect();
    let fit = |lo: f64, hi: f64| -> Option<f32> {
        let pts: Vec<(f64, f64)> = db
            .iter()
            .enumerate()
            .filter(|(_, &d)| d <= lo && d >= hi)
            .map(|(i, &d)| (i as f64 / sr as f64, d))
            .collect();
        if pts.len() < 10 {
            return None;
        }
        let n = pts.len() as f64;
        let (mx, my) = (
            pts.iter().map(|p| p.0).sum::<f64>() / n,
            pts.iter().map(|p| p.1).sum::<f64>() / n,
        );
        let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
        let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
        Some((-60.0 / (sxy / sxx)) as f32)
    };
    fit(-5.0, -35.0)
        .or_else(|| fit(-5.0, -25.0))
        .unwrap_or(f32::NAN)
}

/// Normalised echo density (Abel & Huang 2006), 20 ms windows: time (ms) it first reaches 0.9.
fn ned_time(h: &[f32], sr: f32) -> f32 {
    let w = (0.02 * sr) as usize;
    let hop = (0.001 * sr) as usize;
    let erfc = 0.317_310_5f32;
    let mut i = 0;
    while i + w < h.len() {
        let s = &h[i..i + w];
        let sd = (s.iter().map(|x| x * x).sum::<f32>() / w as f32).sqrt();
        if sd > 0.0 {
            let frac = s.iter().filter(|x| x.abs() > sd).count() as f32 / w as f32;
            if frac / erfc >= 0.9 {
                return (i + w / 2) as f32 / sr * 1000.0;
            }
        }
        i += hop;
    }
    f32::NAN
}

/// Modal flatness of the tail spectrum: (Wiener flatness 300 Hz–6 kHz, 99.9th percentile and
/// max of bin power over its ±1/6-octave median, dB). Gaussian noise gives ≈ 0.56 and ≈ 10 / 13 dB.
fn flatness(h: &[f32], sr: f32) -> (f32, f32, f32) {
    let start = (0.05 * sr) as usize;
    let n = 1 << 16;
    let mut buf = vec![0f32; n];
    for (i, v) in buf.iter_mut().enumerate() {
        *v = *h.get(start + i).unwrap_or(&0.0);
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let mut spec = fft.make_output_vec();
    fft.process(&mut buf, &mut spec).unwrap();
    let pw: Vec<f32> = spec.iter().map(|c| c.norm_sqr() + 1e-30).collect();
    let bin = |f: f32| (f / sr * n as f32) as usize;
    let (a, b) = (bin(300.0), bin(6000.0));
    let lg: f64 = pw[a..b].iter().map(|p| (*p as f64).ln()).sum::<f64>() / (b - a) as f64;
    let ar: f64 = pw[a..b].iter().map(|p| *p as f64).sum::<f64>() / (b - a) as f64;
    let sfm = (lg.exp() / ar) as f32;
    let mut ratios = Vec::with_capacity(b - a);
    let mut win = Vec::new();
    for k in a..b {
        let lo = (k as f32 / 1.122) as usize;
        let hi = ((k as f32 * 1.122) as usize).min(pw.len() - 1);
        win.clear();
        win.extend_from_slice(&pw[lo..=hi]);
        let m = win.len() / 2;
        let med = *win
            .select_nth_unstable_by(m, |x, y| x.partial_cmp(y).unwrap())
            .1;
        ratios.push(10.0 * (pw[k] / med).log10());
    }
    ratios.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let p999 = ratios[(ratios.len() as f32 * 0.999) as usize];
    (sfm, p999, *ratios.last().unwrap())
}

fn corr(a: &[f32], b: &[f32]) -> f32 {
    let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        ab += (*x as f64) * (*y as f64);
        aa += (*x as f64).powi(2);
        bb += (*y as f64).powi(2);
    }
    (ab / (aa * bb).sqrt().max(1e-30)) as f32
}

fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect()
}

fn cpu_us(p: &mut dyn Processor, sr: f32) -> f64 {
    let x = noise((sr * 10.0) as usize, 7);
    let x: Vec<f32> = x.iter().map(|v| v * 0.3).collect();
    let t = Instant::now();
    let _ = run(p, &x, &x, sr);
    t.elapsed().as_secs_f64() * 1e6 / (x.len() / BLOCK) as f64
}

// ---------------------------------------------------------------- true-peak meter (independent of the limiter)

/// 8× oversampled peak with a 384-tap Kaiser sinc (β = 10).
fn true_peak(x: &[f32], lim: f32) -> (f32, usize) {
    const OS: usize = 8;
    const T: usize = 48;
    let centre = (OS * T / 2) as f64;
    let i0 = |x: f64| {
        let (mut s, mut t) = (1.0, 1.0);
        for k in 1..40 {
            t *= (x / 2.0) / k as f64;
            s += t * t;
        }
        s
    };
    let mut h = vec![[0f32; T]; OS];
    for (p, row) in h.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            let m = (OS * j + p) as f64;
            let t = (m - centre) / OS as f64;
            let sinc = if t == 0.0 {
                1.0
            } else {
                (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
            };
            let r = (m - centre) / centre;
            *v = (sinc * i0(10.0 * (1.0 - r * r).max(0.0).sqrt()) / i0(10.0)) as f32;
        }
    }
    let mut pk = 0f32;
    let mut over = 0;
    for n in T..x.len() {
        for row in &h {
            let mut s = 0f32;
            for (j, c) in row.iter().enumerate() {
                s += c * x[n - j];
            }
            pk = pk.max(s.abs());
            if s.abs() > lim {
                over += 1;
            }
        }
    }
    (pk, over)
}

/// Oversampled points above the ceiling (+0.01 dB tolerance for the meter itself).
fn overs(x: &[f32], ceil: f32) -> (f32, usize) {
    true_peak(x, ceil * 10f32.powf(0.01 / 20.0))
}

fn thd(y: &[f32], f0: f32, sr: f32) -> f32 {
    let n = 1 << 16;
    let s = &y[y.len() - n..];
    let mut buf: Vec<f32> = s
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let t = 2.0 * std::f32::consts::PI * i as f32 / n as f32;
            v * (0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos()
                - 0.01168 * (3.0 * t).cos())
        })
        .collect();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let mut spec = fft.make_output_vec();
    fft.process(&mut buf, &mut spec).unwrap();
    let e = |f: f32| {
        let k = (f / sr * n as f32).round() as usize;
        (k.saturating_sub(4)..=(k + 4).min(spec.len() - 1))
            .map(|i| spec[i].norm_sqr())
            .sum::<f32>()
    };
    let fund = e(f0);
    let harm: f32 = (2..=10)
        .map(|k| f0 * k as f32)
        .filter(|f| *f < sr * 0.45)
        .map(e)
        .sum();
    (harm / fund).sqrt() * 100.0
}

fn rms_db(x: &[f32]) -> f32 {
    20.0 * ((x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt() as f32)
        .max(1e-9)
        .log10()
}

fn read_wav(path: &std::path::Path) -> (Vec<f32>, f32) {
    let mut r = hound::WavReader::open(path).expect("wav");
    let spec = r.spec();
    let ch = spec.channels as usize;
    let s: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r
            .samples::<i32>()
            .map(|v| v.unwrap() as f32 / (1u32 << (spec.bits_per_sample - 1)) as f32)
            .collect(),
        hound::SampleFormat::Float => r.samples::<f32>().map(|v| v.unwrap()).collect(),
    };
    (
        s.chunks(ch).map(|c| c[0]).collect(),
        spec.sample_rate as f32,
    )
}

fn write_wav(path: &std::path::Path, l: &[f32], r: &[f32], sr: f32) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: sr as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for (a, b) in l.iter().zip(r) {
        w.write_sample(*a).unwrap();
        w.write_sample(*b).unwrap();
    }
    w.finalize().unwrap();
}

fn reverb_report(sr: f32) {
    println!("\n## Reverb @ {sr} Hz");
    let bands = [125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];
    // new: open input filters so the band decays can be read cleanly
    for (name, space, size, tone, bass) in [
        ("Plate", 0.0, 1.0, 0.0, 0.8),
        ("Room", 1.0, 0.6, 0.0, 1.0),
        ("Hall", 2.0, 2.0, -20.0, 0.8),
        ("Ambience", 3.0, 0.4, 0.0, 1.0),
    ] {
        let mut pb = preset_block(FxKind::Reverb, "Vocal Plate");
        let v = &mut pb.values;
        v[reverb_v2::idx::SPACE] = space;
        v[reverb_v2::idx::SIZE] = size;
        v[reverb_v2::idx::TONE] = tone;
        v[reverb_v2::idx::BASS] = bass;
        v[reverb_v2::idx::LO] = 20.0;
        v[reverb_v2::idx::HI] = 16000.0;
        let (l, r) = ir_new(&pb, sr, (size * 3.0).max(2.0));
        let mono: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a + b).collect();
        let mut row = String::new();
        let mut err_max = 0f32;
        for f in bands {
            let m = rt60(&bandpass(&mono, f, sr), sr);
            let d = reverb_v2::design_rt60(&pb.values, f, sr);
            err_max = err_max.max(((m - d) / d).abs());
            row += &format!(" {f:>5}:{m:.2}/{d:.2}");
        }
        println!(
            "new {name:<8} T60 {size}s  measured/design per band:{row}  max err {:.0}%",
            err_max * 100.0
        );
    }
    for (name, size, hi) in [("old size1.0", 1.0, 7000.0), ("old size2.2", 2.2, 8000.0)] {
        let (l, r) = ir_old(size, 25.0, 20.0, hi, sr, (size * 3.0).max(2.0));
        let mono: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a + b).collect();
        let mut row = String::new();
        for f in bands {
            row += &format!(" {f:>5}:{:.2}", rt60(&bandpass(&mono, f, sr), sr));
        }
        println!("{name:<12} nominal {size}s  measured per band:{row}");
    }
    // character metrics at the vocal settings
    println!("\n| reverb | NED≥0.9 at (ms) | flatness (Wiener) | ring peak p99.9 / max (dB) | L/R corr tail | CPU µs/128 |");
    println!("|---|---|---|---|---|---|");
    let measure = |label: &str, l: Vec<f32>, r: Vec<f32>, cpu: f64| {
        let mono: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a + b).collect();
        let (sfm, p999, mx) = flatness(&mono, sr);
        let a = (0.05 * sr) as usize;
        let b = (0.8 * sr) as usize;
        println!(
            "| {label} | {:.0} | {sfm:.3} | {p999:.1} / {mx:.1} | {:.3} | {cpu:.1} |",
            ned_time(&mono, sr),
            corr(&l[a..b], &r[a..b])
        );
    };
    let (l, r) = ir_old(1.0, 25.0, 300.0, 7000.0, sr, 2.5);
    let mut old = OldReverb::default();
    old.prepare(sr, BLOCK);
    old.set_params(&block(&[1.0, 25.0, 300.0, 7000.0, 100.0, 22.0]));
    measure("old (1.0 s, 300 Hz–7 kHz)", l, r, cpu_us(&mut old, sr));
    for name in ["Vocal Plate", "Vocal Room", "Vocal Hall", "Ambience"] {
        let pb = preset_block(FxKind::Reverb, name);
        let (l, r) = ir_new(&pb, sr, 2.5);
        let mut p = ReverbV2::default();
        p.prepare(sr, BLOCK);
        p.set_params(&pb);
        measure(&format!("new {name}"), l, r, cpu_us(&mut p, sr));
    }
    // white noise reference for the flatness columns
    let nz = noise((sr * 2.0) as usize, 3);
    let (sfm, p999, mx) = flatness(&nz, sr);
    println!("| (white noise reference) | – | {sfm:.3} | {p999:.1} / {mx:.1} | – | – |");
}

fn limiter_report(sr: f32, mixes: &[Vec<f32>], mix_sr: f32) {
    println!("\n## Limiter (ceiling −1 dBTP; old: same ceiling as sample-peak)");
    let ceil = 10f32.powf(-1.0 / 20.0);
    let mut sigs: Vec<(String, Vec<f32>, f32, f32)> = Vec::new();
    let n = (sr * 2.0) as usize;
    // fs/4 sine at 45° phase: samples at ±0.707 of the true peak → classic +3 dB inter-sample over
    sigs.push((
        "fs/4 sine 45° bursts".into(),
        (0..n)
            .map(|i| {
                if (i / 4800) % 2 == 0 {
                    (std::f32::consts::FRAC_PI_2 * i as f32 + 0.785).sin() * 0.9
                } else {
                    0.0
                }
            })
            .collect(),
        sr,
        6.0,
    ));
    sigs.push((
        "11 kHz sine bursts".into(),
        (0..n)
            .map(|i| {
                if (i / 2400) % 3 == 0 {
                    (2.0 * std::f32::consts::PI * 11000.0 * i as f32 / sr).sin() * 0.8
                } else {
                    0.0
                }
            })
            .collect(),
        sr,
        6.0,
    ));
    // synthetic drum loop: kick (pitch-swept sine) + snare (noise burst), 120 bpm 8ths
    let nz = noise(n, 11);
    let drums: Vec<f32> = (0..n)
        .map(|i| {
            let t = (i % (sr as usize / 4)) as f32 / sr;
            let beat = (i / (sr as usize / 4)) % 4;
            let kick = if beat.is_multiple_of(2) {
                (2.0 * std::f32::consts::PI * (50.0 * t + 400.0 * (1.0 - (-t * 30.0).exp()) / 30.0))
                    .sin()
                    * (-t * 8.0).exp()
            } else {
                0.0
            };
            let snare = if beat % 2 == 1 {
                nz[i] * (-t * 20.0).exp() * 0.8
            } else {
                0.0
            };
            (kick + snare) * 0.7
        })
        .collect();
    sigs.push(("synthetic drums".into(), drums, sr, 9.0));
    for (i, m) in mixes.iter().enumerate() {
        sigs.push((format!("MUSDB mix{i} +8 dB"), m.clone(), mix_sr, 8.0));
    }
    println!("| signal | old max TP (dBTP) | old overs | new max TP (dBTP) | new overs | old RMS | new RMS |");
    println!("|---|---|---|---|---|---|---|");
    let (mut tot_old, mut tot_new) = (0usize, 0usize);
    for (name, x, fs, gain) in &sigs {
        let mut o = OldLimiter::default();
        o.prepare(*fs, BLOCK);
        o.set_params(&block(&[*gain, -1.0]));
        let (ol, _) = run(&mut o, x, x, *fs);
        let mut nw = LimiterV2::default();
        nw.prepare(*fs, BLOCK);
        nw.set_params(&preset_v(*gain));
        let (nl, _) = run(&mut nw, x, x, *fs);
        let (tpo, oo) = overs(&ol, ceil);
        let (tpn, on) = overs(&nl, ceil);
        tot_old += oo;
        tot_new += on;
        if nw.clamped > 0 {
            eprintln!("  ({name}: safety clamp touched {} samples)", nw.clamped);
        }
        println!(
            "| {name} | {:.2} | {oo} | {:.2} | {on} | {:.1} | {:.1} |",
            20.0 * tpo.log10(),
            20.0 * tpn.log10(),
            rms_db(&ol),
            rms_db(&nl)
        );
    }
    println!("total overs: old {tot_old}, new {tot_new}");
    // THD at ~6 dB gain reduction, steady sines
    println!("\n| THD at 6 dB GR | old | new |\n|---|---|---|");
    for f0 in [50.0, 100.0, 1000.0] {
        let n = (sr * 3.0) as usize;
        let x: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * f0 * i as f32 / sr).sin() * ceil * 2.0)
            .collect();
        let mut o = OldLimiter::default();
        o.prepare(sr, BLOCK);
        o.set_params(&block(&[0.0, -1.0]));
        let (ol, _) = run(&mut o, &x, &x, sr);
        let mut nw = LimiterV2::default();
        nw.prepare(sr, BLOCK);
        nw.set_params(&preset_v(0.0));
        let (nl, _) = run(&mut nw, &x, &x, sr);
        println!(
            "| {f0} Hz | {:.2} % | {:.2} % |",
            thd(&ol, f0, sr),
            thd(&nl, f0, sr)
        );
    }
    // CPU
    let mut o = OldLimiter::default();
    o.prepare(sr, BLOCK);
    o.set_params(&block(&[6.0, -1.0]));
    let mut nw = LimiterV2::default();
    nw.prepare(sr, BLOCK);
    nw.set_params(&preset_v(6.0));
    println!(
        "CPU µs/128 frames: old {:.1}, new {:.1}",
        cpu_us(&mut o, sr),
        cpu_us(&mut nw, sr)
    );
}

fn preset_v(gain: f32) -> ParamBlock {
    let mut pb = param_block(&new_fx(1, FxKind::Limit, 120.0));
    pb.values[wave_engine::dsp::limiter_v2::idx::GAIN] = gain;
    pb.values[wave_engine::dsp::limiter_v2::idx::CEIL] = -1.0;
    pb
}

fn render(dir: &std::path::Path) -> Vec<Vec<f32>> {
    let src = dir.join("src");
    let (voc, sr) = read_wav(&src.join("vocal_dry.wav"));
    write_wav(&dir.join("01_vocal_dry.wav"), &voc, &voc, sr);
    // wet-only renders, then the same wet level (the new Vocal Plate's own 15 % mix) for both
    let pb = preset_block(FxKind::Reverb, "Vocal Plate");
    let mut wet_pb = pb;
    wet_pb.values[reverb_v2::idx::MIX] = 100.0;
    let mut nw = ReverbV2::default();
    nw.prepare(sr, BLOCK);
    nw.set_params(&wet_pb);
    let (nl, nr) = run(&mut nw, &voc, &voc, sr);
    let mut od = OldReverb::default();
    od.prepare(sr, BLOCK);
    od.set_params(&block(&[1.0, 25.0, 300.0, 7000.0, 100.0, 100.0]));
    let (mut ol, mut or) = run(&mut od, &voc, &voc, sr);
    for i in 0..voc.len() {
        ol[i] -= 0.5 * voc[i];
        or[i] -= 0.5 * voc[i];
    }
    let wet_g = 0.3; // 15 % mix → wet gain 0.3, dry 1
    let k = 10f32.powf((rms_db(&nl) - rms_db(&ol)) / 20.0);
    let mixl =
        |w: &[f32], g: f32| -> Vec<f32> { voc.iter().zip(w).map(|(d, x)| d + x * g).collect() };
    write_wav(
        &dir.join("02_vocal_old_reverb.wav"),
        &mixl(&ol, wet_g * k),
        &mixl(&or, wet_g * k),
        sr,
    );
    write_wav(
        &dir.join("03_vocal_new_vocal_plate.wav"),
        &mixl(&nl, wet_g),
        &mixl(&nr, wet_g),
        sr,
    );
    println!(
        "\nrender: old wet scaled by {:.2} dB to match the new wet level",
        20.0 * k.log10()
    );
    for (i, name) in [
        "Vocal Room",
        "Vocal Hall",
        "Ambience",
        "Ballada — długi plate",
    ]
    .iter()
    .enumerate()
    {
        let mut p = ReverbV2::default();
        p.prepare(sr, BLOCK);
        p.set_params(&preset_block(FxKind::Reverb, name));
        let (l, r) = run(&mut p, &voc, &voc, sr);
        let fname = format!(
            "{:02}_vocal_new_{}.wav",
            4 + i,
            name.split_whitespace()
                .last()
                .unwrap()
                .to_lowercase()
                .replace('—', "")
        );
        write_wav(&dir.join(fname), &l, &r, sr);
    }
    // limiter on beat + vocal
    let mut mixes = Vec::new();
    let mut mix_sr = 44100.0;
    for i in 0..5 {
        let p = src.join(format!("mix{i}.wav"));
        if p.exists() {
            let (m, s) = read_wav(&p);
            mix_sr = s;
            mixes.push(m);
        }
    }
    if let Some(m) = mixes.first() {
        write_wav(&dir.join("10_mix_unlimited.wav"), m, m, mix_sr);
        let mut o = OldLimiter::default();
        o.prepare(mix_sr, BLOCK);
        o.set_params(&block(&[8.0, -1.0]));
        let (l, r) = run(&mut o, m, m, mix_sr);
        write_wav(&dir.join("11_mix_old_limiter_+8dB.wav"), &l, &r, mix_sr);
        let mut nw = LimiterV2::default();
        nw.prepare(mix_sr, BLOCK);
        nw.set_params(&preset_v(8.0));
        let (l, r) = run(&mut nw, m, m, mix_sr);
        write_wav(&dir.join("12_mix_new_limiter_+8dB.wav"), &l, &r, mix_sr);
    }
    mixes
}

fn main() {
    let dir = std::env::args().nth(1).map(std::path::PathBuf::from);
    for sr in [44100.0, 96000.0] {
        reverb_report(sr);
    }
    let mixes = dir.as_deref().map(render).unwrap_or_default();
    limiter_report(48000.0, &mixes, 44100.0);
}
