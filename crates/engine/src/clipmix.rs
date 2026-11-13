//! Mixing an audio clip into a track buffer for one block.
//!
//! The clip's samples inside the block are found once, split into fade-in / body /
//! fade-out segments (the gain is constant in the body) and read with an incrementally
//! advanced source position; when the file plays at the output rate the interpolation
//! weights are the same for the whole block. Allocation-free.

use crate::rt::ClipRt;
use crate::sources::Audio;

/// Mixes `c` (an audio clip) into `out_l`/`out_r`, which start at the first sample of the
/// block; the block's first sample sits at beat `b0`, `bps` beats per sample, `spb` seconds
/// per beat.
pub(crate) fn mix_audio_clip(c: &ClipRt, a: &Audio, b0: f64, bps: f64, spb: f64, out_l: &mut [f32], out_r: &mut [f32]) {
    let n = out_l.len().min(out_r.len());
    // samples of this block inside the clip: start <= beat < end (the same per-sample test
    // as always, so the edges land on the same samples)
    let mut i0 = (((c.start - b0) / bps).ceil().max(0.0) as usize).min(n);
    let mut i1 = (((c.end - b0) / bps).ceil().max(0.0) as usize).min(n);
    while i1 > i0 && b0 + (i1 - 1) as f64 * bps >= c.end {
        i1 -= 1;
    }
    while i0 < i1 && b0 + i0 as f64 * bps < c.start {
        i0 += 1;
    }
    if i1 <= i0 {
        return;
    }
    let dur = (c.end - c.start) * spb;
    let g = 10f32.powf(c.gain / 20.0);
    let pans = |p: f32| (if p > 0.0 { 1.0 - p } else { 1.0 }, if p < 0.0 { 1.0 + p } else { 1.0 });
    let (pl, pr) = pans(c.pan);
    // an automated gain or pan glides from the previous block's value across this one
    let glide = c.gain0 != c.gain || c.pan0 != c.pan;
    let (g0, (pl0, pr0)) = (10f32.powf(c.gain0 / 20.0), pans(c.pan0));
    let inv_n = 1.0 / n.max(1) as f32;
    let gain_at = |i: usize| if glide { g0 + (g - g0) * (i as f32 * inv_n) } else { g };
    let t_of = |i: usize| (b0 + i as f64 * bps - c.start) * spb;
    let (fi, fo) = (c.fade_in as f64, c.fade_out as f64);
    let env_of = |i: usize| {
        let t = t_of(i);
        let mut env = gain_at(i);
        if c.fade_in > 0.0 && t < fi {
            env *= (t / fi) as f32;
        }
        let left = dur - t;
        if c.fade_out > 0.0 && left < fo {
            env *= (left / fo) as f32;
        }
        env
    };
    // first sample past the fade-in, first sample inside the fade-out (both monotone in i)
    let mut fi_end = i0;
    if c.fade_in > 0.0 {
        fi_end = ((((c.start - b0) + fi / spb) / bps).ceil().max(i0 as f64) as usize).min(i1);
        while fi_end > i0 && t_of(fi_end - 1) >= fi {
            fi_end -= 1;
        }
        while fi_end < i1 && t_of(fi_end) < fi {
            fi_end += 1;
        }
    }
    let mut fo_start = i1;
    if c.fade_out > 0.0 {
        fo_start = ((((c.start - b0) + (dur - fo) / spb) / bps).ceil().max(i0 as f64) as usize).min(i1);
        while fo_start > i0 && dur - t_of(fo_start - 1) < fo {
            fo_start -= 1;
        }
        while fo_start < i1 && dur - t_of(fo_start) >= fo {
            fo_start += 1;
        }
    }
    let fsr = a.sr as f64;
    // `rate` is exactly 1 unless the clip is resampled, so unstretched clips read the same
    // positions (and keep the on-grid fast path) as before it existed
    let step = bps * spb * fsr * c.rate;
    let src_at = |i: usize| (c.offset + (b0 + i as f64 * bps - c.start)) * spb * fsr * c.rate;
    if glide {
        // automated: every sample its own gain and pan (the fades on top)
        let pan_at = |i: usize| {
            let t = i as f32 * inv_n;
            (pl0 + (pl - pl0) * t, pr0 + (pr - pr0) * t)
        };
        add_run_stereo(a, src_at(i0), step, &mut out_l[i0..i1], &mut out_r[i0..i1], |k| {
            let (e, (a, b)) = (env_of(i0 + k), pan_at(i0 + k));
            (e * a, e * b)
        });
        return;
    }
    if fi_end > fo_start {
        // the fades overlap (a very short clip): every sample gets its full envelope
        add_run(a, src_at(i0), step, &mut out_l[i0..i1], &mut out_r[i0..i1], |k| env_of(i0 + k), pl, pr);
        return;
    }
    if fi_end > i0 {
        add_run(a, src_at(i0), step, &mut out_l[i0..fi_end], &mut out_r[i0..fi_end], |k| env_of(i0 + k), pl, pr);
    }
    if fo_start > fi_end {
        add_run(a, src_at(fi_end), step, &mut out_l[fi_end..fo_start], &mut out_r[fi_end..fo_start], |_| g, pl, pr);
    }
    if i1 > fo_start {
        add_run(a, src_at(fo_start), step, &mut out_l[fo_start..i1], &mut out_r[fo_start..i1], |k| env_of(fo_start + k), pl, pr);
    }
}

/// [`add_run`] with a separate gain per side and sample (an automated gain or pan gliding).
fn add_run_stereo(a: &Audio, src0: f64, step: f64, out_l: &mut [f32], out_r: &mut [f32], env: impl Fn(usize) -> (f32, f32)) {
    for k in 0..out_l.len() {
        let (x, y) = a.sample(src0 + k as f64 * step);
        let (el, er) = env(k);
        out_l[k] += x * el;
        out_r[k] += y * er;
    }
}

/// Adds `env(k)`-scaled samples read at `src0 + k * step` (the file's frames, cubic
/// interpolation, silence outside the file) into the slices.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn add_run(a: &Audio, src0: f64, step: f64, out_l: &mut [f32], out_r: &mut [f32], env: impl Fn(usize) -> f32, pl: f32, pr: f32) {
    let m = out_l.len();
    let frames = a.frames as f64;
    let last = src0 + (m - 1) as f64 * step;
    if src0 < 0.0 || last >= frames {
        // an edge of the file inside the run: the careful per-sample read
        for k in 0..m {
            let (x, y) = a.sample(src0 + k as f64 * step);
            let e = env(k);
            out_l[k] += x * e * pl;
            out_r[k] += y * e * pr;
        }
        return;
    }
    let d = &a.data[..];
    if (step - 1.0).abs() < 1e-12 {
        // file at the output rate: one fractional position for the whole run
        let i0 = src0 as usize;
        let x = (src0 - i0 as f64) as f32;
        if x < 1e-7 {
            // on the sample grid the interpolation returns the sample itself
            for k in 0..m {
                let b = (i0 + k + 1) * 2;
                let e = env(k);
                out_l[k] += d[b] * e * pl;
                out_r[k] += d[b + 1] * e * pr;
            }
        } else {
            let x2 = x * x;
            let x3 = x2 * x;
            let w0 = -0.5 * x + x2 - 0.5 * x3;
            let w1 = 1.0 - 2.5 * x2 + 1.5 * x3;
            let w2 = 0.5 * x + 2.0 * x2 - 1.5 * x3;
            let w3 = -0.5 * x2 + 0.5 * x3;
            for k in 0..m {
                // taps start one frame before the content frame (`taps` base = index * channels)
                let b = (i0 + k) * 2;
                let s = &d[b..b + 8];
                let xl = w0 * s[0] + w1 * s[2] + w2 * s[4] + w3 * s[6];
                let xr = w0 * s[1] + w1 * s[3] + w2 * s[5] + w3 * s[7];
                let e = env(k);
                out_l[k] += xl * e * pl;
                out_r[k] += xr * e * pr;
            }
        }
        return;
    }
    // resampling: integer index and fraction advanced together (no per-sample float->int)
    let step_i = step as usize;
    let step_f = step - step_i as f64;
    let mut ip = src0 as usize;
    let mut fr = src0 - ip as f64;
    for k in 0..m {
        let x = fr as f32;
        let b = ip * 2;
        let s = &d[b..b + 8];
        let x2 = x * x;
        let x3 = x2 * x;
        let w0 = -0.5 * x + x2 - 0.5 * x3;
        let w1 = 1.0 - 2.5 * x2 + 1.5 * x3;
        let w2 = 0.5 * x + 2.0 * x2 - 1.5 * x3;
        let w3 = -0.5 * x2 + 0.5 * x3;
        let xl = w0 * s[0] + w1 * s[2] + w2 * s[4] + w3 * s[6];
        let xr = w0 * s[1] + w1 * s[3] + w2 * s[5] + w3 * s[7];
        let e = env(k);
        out_l[k] += xl * e * pl;
        out_r[k] += xr * e * pr;
        fr += step_f;
        let carry = (fr >= 1.0) as usize;
        fr -= carry as f64;
        ip += step_i + carry;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::ClipSrc;
    use std::sync::Arc;

    /// The per-sample implementation this module replaced.
    #[allow(clippy::too_many_arguments)]
    fn reference(c: &ClipRt, a: &Audio, b0: f64, bps: f64, spb: f64, n: usize, out_l: &mut [f32], out_r: &mut [f32]) {
        let dur = (c.end - c.start) * spb;
        let g = 10f32.powf(c.gain / 20.0);
        let (pl, pr) = (if c.pan > 0.0 { 1.0 - c.pan } else { 1.0 }, if c.pan < 0.0 { 1.0 + c.pan } else { 1.0 });
        let fsr = a.sr as f64;
        for i in 0..n {
            let bt = b0 + i as f64 * bps;
            if bt < c.start || bt >= c.end {
                continue;
            }
            let rel = bt - c.start;
            let t = rel * spb;
            let mut env = g;
            if c.fade_in > 0.0 && t < c.fade_in as f64 {
                env *= (t / c.fade_in as f64) as f32;
            }
            let left = dur - t;
            if c.fade_out > 0.0 && left < c.fade_out as f64 {
                env *= (left / c.fade_out as f64) as f32;
            }
            let src = (c.offset + rel) * spb * fsr * c.rate;
            let (x, y) = a.sample(src);
            out_l[i] += x * env * pl;
            out_r[i] += y * env * pr;
        }
    }

    fn audio(sr: u32, secs: f32) -> Arc<Audio> {
        let n = (sr as f32 * secs) as usize;
        let mut seed = 12345u32;
        let data: Vec<f32> = (0..n * 2)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                ((i / 2) as f32 * 0.05).sin() * 0.5 + noise * 0.2 + if i % 2 == 0 { 0.1 } else { -0.1 }
            })
            .collect();
        Arc::new(Audio::from_stereo(data, sr, 2))
    }

    #[test]
    fn an_automated_gain_glides_across_the_block() {
        let a = audio(48000, 1.0);
        let spb = 0.5;
        let bps = 1.0 / (48000.0 * spb);
        let mut c = ClipRt {
            id: 1,
            track: 0,
            start: 0.0,
            end: 4.0,
            offset: 0.0,
            rate: 1.0,
            gain: 0.0,
            pan: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            src: ClipSrc::Audio(a.clone()),
            gain0: -24.0,
            pan0: 0.0,
        };
        let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
        mix_audio_clip(&c, &a, 0.5, bps, spb, &mut l, &mut r);
        // the reference at the old and the new gain bound the glide; it starts at the old one
        let (mut lo, mut lo_r, mut hi, mut hi_r) = (vec![0f32; 512], vec![0f32; 512], vec![0f32; 512], vec![0f32; 512]);
        c.gain0 = -24.0;
        c.gain = -24.0;
        mix_audio_clip(&c, &a, 0.5, bps, spb, &mut lo, &mut lo_r);
        c.gain0 = 0.0;
        c.gain = 0.0;
        mix_audio_clip(&c, &a, 0.5, bps, spb, &mut hi, &mut hi_r);
        assert!((l[0] - lo[0]).abs() < 1e-6, "starts at the old gain");
        for i in 0..512 {
            let (x0, x1) = (lo[i].min(hi[i]), lo[i].max(hi[i]));
            assert!(l[i] >= x0 - 1e-6 && l[i] <= x1 + 1e-6, "sample {i} outside the glide");
        }
        let e = |x: &[f32]| x[448..].iter().map(|v| v * v).sum::<f32>();
        assert!(e(&l) > e(&lo) * 30.0, "ends near the new gain");
    }

    #[test]
    fn matches_the_per_sample_reference() {
        let out_sr = 48000.0f64;
        let mut seed = 99u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f64 / (1u32 << 24) as f64
        };
        let mut worst = 0f32;
        let mut cases = 0;
        for &file_sr in &[48000u32, 44100, 96000, 22050] {
            let a = audio(file_sr, 3.0);
            for _ in 0..60 {
                let bpm = 60.0 + rnd() * 120.0;
                let spb = 60.0 / bpm;
                let bps = 1.0 / (spb * out_sr);
                let block = [64usize, 256, 512, 1024][(rnd() * 4.0) as usize % 4];
                let start = rnd() * 4.0;
                let len = [0.002, 0.05, 0.5, 1.5, 4.0][(rnd() * 5.0) as usize % 5] / spb;
                let fade = |r: f64| if r < 0.3 { 0.0 } else { (r * 0.2) as f32 };
                let mut c = ClipRt {
                    id: 1,
                    track: 0,
                    start,
                    end: start + len,
                    offset: rnd() * 0.5 / spb,
                    rate: 1.0,
                    gain: (rnd() * 12.0 - 6.0) as f32,
                    pan: if rnd() < 0.5 { 0.0 } else { (rnd() * 2.0 - 1.0) as f32 },
                    fade_in: fade(rnd()),
                    fade_out: fade(rnd()),
                    src: ClipSrc::Audio(a.clone()),
                    gain0: 0.0,
                    pan0: 0.0,
                };
                (c.gain0, c.pan0) = (c.gain, c.pan);
                // a third of the clips resampled (stretched by 0.25..4 without keeping pitch)
                if rnd() < 0.33 {
                    c.rate = 0.25 + rnd() * 3.75;
                }
                // blocks around the clip: before, over the start, inside, over the end, after
                let first = (start - 2.0 * block as f64 * bps).max(0.0);
                let mut b0 = first;
                for _ in 0..((len / (block as f64 * bps)) as usize + 6).min(400) {
                    let (mut l1, mut r1) = (vec![0.25f32; block], vec![-0.25f32; block]);
                    let (mut l2, mut r2) = (l1.clone(), r1.clone());
                    reference(&c, &a, b0, bps, spb, block, &mut l1, &mut r1);
                    mix_audio_clip(&c, &a, b0, bps, spb, &mut l2, &mut r2);
                    for i in 0..block {
                        worst = worst.max((l1[i] - l2[i]).abs()).max((r1[i] - r2[i]).abs());
                    }
                    b0 += block as f64 * bps;
                    cases += 1;
                }
            }
        }
        assert!(cases > 1000);
        assert!(worst < 2e-6, "clip mix differs from the per-sample reference by {worst} (> -113 dB)");
    }

    /// Prints µs per sounding clip per 512-frame block (`cargo test -p wave-engine --lib clipmix -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn cost_per_clip() {
        for &file_sr in &[48000u32, 44100] {
            let a = audio(file_sr, 10.0);
            let spb = 60.0 / 120.0;
            let bps = 1.0 / (spb * 48000.0);
            let c = ClipRt {
                id: 1,
                track: 0,
                start: 0.0,
                end: 16.0,
                offset: 0.3,
                rate: 1.0,
                gain: -3.0,
                pan: 0.2,
                fade_in: 0.01,
                fade_out: 0.01,
                src: ClipSrc::Audio(a.clone()),
                gain0: -3.0,
                pan0: 0.2,
            };
            let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
            let reps = 20000;
            let t = std::time::Instant::now();
            for k in 0..reps {
                reference(&c, &a, 1.0 + (k % 100) as f64 * 512.0 * bps, bps, spb, 512, &mut l, &mut r);
            }
            let old = t.elapsed().as_secs_f64() / reps as f64 * 1e6;
            let t = std::time::Instant::now();
            for k in 0..reps {
                mix_audio_clip(&c, &a, 1.0 + (k % 100) as f64 * 512.0 * bps, bps, spb, &mut l, &mut r);
            }
            let new = t.elapsed().as_secs_f64() / reps as f64 * 1e6;
            println!("file {file_sr} Hz: reference {old:.2} us/clip/block, now {new:.2} us/clip/block ({:.1}x)", old / new);
            assert!(l[0].is_finite());
        }
    }
}
