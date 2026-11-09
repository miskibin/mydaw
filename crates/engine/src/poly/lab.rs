//! Measurement helpers for the synth (aliasing, clicks, loudness, probe renders). Not used on
//! the audio thread; shared by the tests and `examples/synth_lab.rs`.

use realfft::RealFftPlanner;

use super::presets::Preset;
use super::{Patch, SynthInst};
use crate::synth::Dest;

/// Alias SNR of a steady tone at `f0`: energy on the harmonic bins over energy everywhere
/// else up to `fmax` (dB). 1 s at `sr`, Blackman-Harris window, ±8 bins per harmonic.
pub fn alias_snr(x: &[f32], f0: f64, sr: f64, fmax: f64) -> f64 {
    let n = x.len();
    let mut buf: Vec<f64> = x
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let t = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
            let w = 0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos();
            v as f64 * w
        })
        .collect();
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(n);
    let mut spec = fft.make_output_vec();
    fft.process(&mut buf, &mut spec).unwrap();
    let hz = sr / n as f64;
    let (mut on, mut off) = (0f64, 0f64);
    for (k, c) in spec.iter().enumerate() {
        let f = k as f64 * hz;
        if f < 15.0 || f > fmax {
            continue;
        }
        let e = c.norm_sqr();
        let h = (f / f0).round();
        if h >= 1.0 && (f - h * f0).abs() <= 8.0 * hz {
            on += e;
        } else {
            off += e;
        }
    }
    10.0 * (on / off.max(1e-300)).log10()
}

/// Click detector: a sample whose second difference is
/// * more than `k` times the largest second difference within 40 ms before it AND within
///   40 ms after it (1 ms either side excluded; one cycle of a 25 Hz bass): periodic sharp features of the sound itself
///   (saw resets, stepped waves) recur in both windows and are not flagged, an isolated
///   discontinuity (a cut, a jump, a step) is;
/// * above −60 dBFS and above 2 % of the local peak level (±40 ms): slope kinks 34 dB under
///   the signal are not counted;
/// * not at an onset from silence (the 40 ms before peak below −60 dBFS): there the attack
///   time (≥ 0.5 ms) is the guard, and a sharp first cycle of a pluck is not a click.
///
/// Returns flagged sample indices (one per 40 ms at most).
pub fn click_flags(x: &[f32], sr: f32, k: f32) -> Vec<usize> {
    let n = x.len();
    if n < 4 {
        return vec![];
    }
    let d: Vec<f32> = (0..n).map(|i| if i < 2 { 0.0 } else { (x[i] - 2.0 * x[i - 1] + x[i - 2]).abs() }).collect();
    let bs = ((0.0005 * sr) as usize).max(1); // 0.5 ms blocks
    let nb = n.div_ceil(bs);
    let bmax: Vec<f32> = (0..nb).map(|b| d[b * bs..((b + 1) * bs).min(n)].iter().fold(0f32, |m, v| m.max(*v))).collect();
    let (near, far) = (2usize, (0.04 * sr) as usize / bs);
    let amax: Vec<f32> = (0..nb).map(|b| x[b * bs..((b + 1) * bs).min(n)].iter().fold(0f32, |m, v| m.max(v.abs()))).collect();
    let mut out: Vec<usize> = vec![];
    for i in 0..n {
        if d[i] <= 1e-3 {
            continue;
        }
        let b = i / bs;
        let past = bmax[b.saturating_sub(far)..b.saturating_sub(near)].iter().fold(0f32, |m, v| m.max(*v));
        let fut = bmax[(b + near).min(nb)..(b + far).min(nb)].iter().fold(0f32, |m, v| m.max(*v));
        let before = amax[b.saturating_sub(far)..b.saturating_sub(near)].iter().fold(0f32, |m, v| m.max(*v));
        let level = amax[b.saturating_sub(far)..(b + far).min(nb)].iter().fold(0f32, |m, v| m.max(*v));
        if before < 1e-3 || d[i] < 0.02 * level {
            continue;
        }
        if d[i] > k * past && d[i] > k * fut && out.last().is_none_or(|&l| i - l > far * bs) {
            out.push(i);
        }
    }
    out
}

/// Integrated loudness (BS.1770, stereo: channel energies summed) and true peak (dBTP).
pub fn loudness(l: &[f32], r: &[f32], sr: f32) -> (f64, f64) {
    let kl: Vec<f64> = crate::autovocal::k_weight(l, sr).iter().map(|v| v * v).collect();
    let kr: Vec<f64> = crate::autovocal::k_weight(r, sr).iter().map(|v| v * v).collect();
    let k: Vec<f64> = kl.iter().zip(&kr).map(|(a, b)| a + b).collect();
    let (w, h) = ((sr * 0.4) as usize, (sr * 0.1) as usize);
    let tp = crate::autovocal::true_peak_db(l).max(crate::autovocal::true_peak_db(r));
    if k.len() < w {
        return (-70.0, tp);
    }
    let mut pre = vec![0.0; k.len() + 1];
    for i in 0..k.len() {
        pre[i + 1] = pre[i] + k[i];
    }
    let z: Vec<f64> = (0..=(k.len() - w) / h).map(|j| (pre[j * h + w] - pre[j * h]) / w as f64).collect();
    let lk = |z: f64| -0.691 + 10.0 * z.max(1e-20).log10();
    let abs: Vec<f64> = z.iter().copied().filter(|&v| lk(v) > -70.0).collect();
    if abs.is_empty() {
        return (-70.0, tp);
    }
    let rel = lk(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
    let g: Vec<f64> = abs.into_iter().filter(|&v| lk(v) > rel).collect();
    (lk(g.iter().sum::<f64>() / g.len().max(1) as f64), tp)
}

/// Renders `secs` of an instance in 64-sample blocks.
pub fn run(s: &mut SynthInst, secs: f32, sr: f32) -> (Vec<f32>, Vec<f32>) {
    let n = (secs * sr) as usize;
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    let mut i = 0;
    while i < n {
        let m = 64.min(n - i);
        s.render_into(&mut l[i..i + m], &mut r[i..i + m], 120.0);
        i += m;
    }
    (l, r)
}

/// The levelling probe: one note (C3 for basses, C4 otherwise), velocity 100, held 2 s,
/// then 2 s of release.
pub fn probe(p: &Preset, sr: f32) -> (Vec<f32>, Vec<f32>) {
    probe_patch(&p.patch, p.probe_key(), sr)
}

pub fn probe_patch(patch: &Patch, key: u8, sr: f32) -> (Vec<f32>, Vec<f32>) {
    let mut s = SynthInst::new(1, *patch, sr);
    s.note(0, (2.0 * sr) as u32, key, 100.0 / 127.0, Dest::Click);
    run(&mut s, 4.0, sr)
}

/// The heaviest patch for CPU measurements: 7-voice unison on both oscillators, LP24 with
/// drive and a modulated cutoff, every global effect on.
pub fn heaviest_patch() -> Patch {
    use super::params::ix;
    let mut p = super::presets::all().into_iter().find(|p| p.name == "Supersaw Lead").map(|p| p.patch).unwrap_or_else(super::params::defaults);
    p[ix::UNI_VOICES] = 3.0;
    p[ix::O2_LEVEL] = 40.0;
    p[ix::FLT_TYPE] = 1.0;
    p[ix::FLT_DRIVE] = 30.0;
    p[ix::FX_DRIVE] = 20.0;
    p[ix::FX_CHORUS] = 30.0;
    p[ix::FX_DELAY] = 20.0;
    p[ix::FX_REVERB] = 30.0;
    p[ix::M2_SRC] = 2.0;
    p[ix::M2_DST] = 7.0;
    p[ix::M2_AMT] = 5.0;
    p
}

/// An instance of [`heaviest_patch`] with 16 notes held for `secs`.
pub fn heaviest_instance(sr: f32, secs: f32) -> SynthInst {
    let mut s = SynthInst::new(1, heaviest_patch(), sr);
    for i in 0..16u32 {
        s.note(0, (secs * sr) as u32, (48 + i * 2) as u8, 0.8, Dest::Click);
    }
    s
}
