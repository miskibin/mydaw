//! Delay: a stereo echo with a processing loop, in the spirit of a tape / BBD unit with digital
//! degradation on tap. The control set follows the classic "everything inside the feedback loop"
//! design (filter, sample-rate and bit reduction, saturation or limiting, diffusion, LFO on time
//! and cutoff); the DSP is our own.
//!
//! ```text
//!  in ─ Input ─┬─ (model routing, pre-delay offset) ─ + ─ ring ─ read (Hermite) ─ loop chain ─┬─ spread ─ Tone ─ Wet ─┐
//!              │                                     └──── × Feedback ────────────────────────┘                      │
//!              └──────────────────────────────────────────── Dry ──────────────────────────────────────────────── + ─ out
//!  loop chain: SVF filter (LP/HP/BP, LFO on cutoff) → sample-rate hold → Limit / Sat → DC blocker
//!              → bit quantiser → diffusion (4 all-passes, blended)
//! ```
//!
//! * The chain sits between the read head and both the output and the feedback path, so the
//!   first echo is already shaped and every pass shapes it again; with model Off there is no
//!   ring at all and the chain processes the input directly (a filter / lo-fi / drive unit).
//! * Time is either tempo-synced (beats, follows `ProcessCtx::bpm` live, so tempo automation
//!   moves the echoes) or free (ms). Keep pitch off: the delay glides towards a new time
//!   (one-pole slew, Smoothing 0 % = jump), which repitches the echoes like tape. Keep pitch on:
//!   two heads crossfade (40 ms) from the old time to the new one, so pitch never bends.
//! * Feedback goes to 125 %. The loop is bounded by construction: the Sat curve never exceeds
//!   its threshold and Limit clips at it, and a ±4 safety clamp plus a NaN guard on the write
//!   keep any setting finite.
//! * Model switches dip the wet path for 10 ms instead of jumping (no click).
//! * No allocation after `prepare`.

use super::{flush, Biquad, FilterType};
use crate::processor::{ParamBlock, ProcessCtx, Processor, METERS};

/// Order of the parameter list in `catalog::params(FxKind::Delay)`.
pub mod idx {
    pub const INPUT: usize = 0;
    pub const SYNC: usize = 1;
    pub const BEATS: usize = 2;
    pub const TIME: usize = 3;
    pub const KEEP: usize = 4;
    pub const SMOOTH: usize = 5;
    pub const OFFSET: usize = 6;
    pub const PAN: usize = 7;
    pub const MODEL: usize = 8;
    pub const SPREAD: usize = 9;
    pub const FB: usize = 10;
    pub const FTYPE: usize = 11;
    pub const CUTOFF: usize = 12;
    pub const RES: usize = 13;
    pub const SRATE: usize = 14;
    pub const BITS: usize = 15;
    pub const DIST: usize = 16;
    pub const LEVEL: usize = 17;
    pub const KNEE: usize = 18;
    pub const SYM: usize = 19;
    pub const DIFF: usize = 20;
    pub const DSPREAD: usize = 21;
    pub const RATE: usize = 22;
    pub const MTIME: usize = 23;
    pub const MCUT: usize = 24;
    pub const MIX: usize = 25;
    pub const TONE: usize = 26;
    pub const DRY: usize = 27;
    pub const COUNT: usize = 28;
}

pub const SYNCS: [&str; 2] = ["Tempo", "ms"];
pub const MODELS: [&str; 4] = ["Mono", "Stereo", "Ping-pong", "Off"];
pub const MONO: usize = 0;
pub const STEREO: usize = 1;
pub const PINGPONG: usize = 2;
pub const OFF: usize = 3;
pub const FILTERS: [&str; 4] = ["Off", "LP", "HP", "BP"];
pub const DISTS: [&str; 2] = ["Limit", "Sat"];
/// Longest echo (and longest per-channel loop), seconds. Longer synced times are clamped.
pub const MAX_DELAY_S: f32 = 4.0;
/// Sample-rate reduction at or above this is off.
pub const SRATE_OFF: f32 = 47_000.0;
/// Bit depth at or above this is off.
pub const BITS_OFF: f32 = 24.0;
/// Peak deviation of the time LFO at 100 %, seconds (wow on long echoes, chorus on short ones).
const MOD_S: f32 = 0.006;
/// Keep pitch: crossfade from the old head to the new one.
const XFADE_S: f32 = 0.04;
/// Model switch: the wet path dips this long down and back up.
const SWITCH_S: f32 = 0.01;
/// Diffusion all-pass lengths at Spread 50 %, ms (right channel ×1.07).
const AP_MS: [f32; 4] = [4.7, 7.3, 11.1, 16.9];
const AP_G: f32 = 0.6;
/// Absolute ceiling of anything written into the loop.
const SAFETY: f32 = 4.0;

/// Seconds of the delay for parameters `beats`/`ms` at `bpm` (`sync` 0 = tempo).
pub fn delay_seconds(sync: f32, beats: f32, ms: f32, bpm: f64) -> f32 {
    let s = if sync < 0.5 {
        let bpm = if bpm > 1.0 { bpm as f32 } else { 120.0 };
        beats * 60.0 / bpm
    } else {
        ms / 1000.0
    };
    s.clamp(0.0005, MAX_DELAY_S)
}

/// Brings the parameters of the first delay (only `time` ms 10..1500, `fb` 0..95, `mix`) to this
/// one, sounding the same: free time, no loop filter, a near-linear Sat at 0 dB, full dry.
/// Returns false when they are already current.
pub fn migrate_params(p: &mut std::collections::BTreeMap<String, f64>) -> bool {
    if p.contains_key("sync") {
        return false;
    }
    let time = p.get("time").copied().unwrap_or(375.0);
    let fb = p.get("fb").copied().unwrap_or(35.0);
    let mix = p.get("mix").copied().unwrap_or(25.0);
    p.clear();
    for d in crate::catalog::params(crate::model::FxKind::Delay) {
        p.insert(d.id.to_owned(), d.def);
    }
    for (k, v) in [
        ("sync", 1.0),
        ("time", time),
        ("fb", fb),
        ("mix", mix),
        ("ftype", 0.0),
        ("knee", 0.0),
        ("dry", 100.0),
        ("smooth", 51.0),
    ] {
        p.insert(k.into(), v);
    }
    true
}

/// Remaps a normalised automation value of the first delay's parameter `param` to this one's
/// range (the first delay had `time` 10..1500 ms log and `fb` 0..95 %).
pub fn migrate_auto_value(param: &str, v: f32) -> f32 {
    match param {
        "time" => {
            let ms = 10.0 * 150f32.powf(v.clamp(0.0, 1.0));
            ms.ln() / 2000f32.ln()
        }
        "fb" => v.clamp(0.0, 1.0) * 95.0 / 125.0,
        _ => v,
    }
}

/// Resonance 0..100 % → SVF damping k (1/Q). 75 % ≈ Q 5.6: enough to ring the loop on its own.
#[inline]
pub fn res_k(res: f32) -> f32 {
    let r = (res / 100.0).clamp(0.0, 1.0);
    (std::f32::consts::SQRT_2 * (1.0 - r).powf(1.5)).max(0.005)
}

/// Tone knob −100..100 → (filter, cutoff Hz); `None` = off (centre).
pub fn tone_filter(tone: f32) -> Option<(FilterType, f32)> {
    let t = (tone / 100.0).clamp(-1.0, 1.0);
    if t.abs() < 0.01 {
        None
    } else if t < 0.0 {
        Some((
            FilterType::Lowpass,
            20_000.0 * (200.0f32 / 20_000.0).powf(-t),
        ))
    } else {
        Some((FilterType::Highpass, 20.0 * (5_000.0f32 / 20.0).powf(t)))
    }
}

/// The loop's waveshaper: `dist` 0 = Limit (hard clip at `th`), 1 = Sat (linear up to
/// `th·(1−w)`, then a tanh knee that never exceeds `th`). Symmetry pushes the negative half's
/// threshold up to 4× (the positive half saturates first: even harmonics).
#[inline]
pub fn shape(x: f32, dist: usize, th: f32, knee: f32, sym: f32) -> f32 {
    if dist == 0 {
        return x.clamp(-th, th);
    }
    let t = if x < 0.0 { th * (1.0 + 3.0 * sym) } else { th };
    let w = 0.02 + 0.88 * knee;
    let u = x.abs() / t;
    let lin = 1.0 - w;
    let y = if u <= lin {
        u
    } else {
        lin + w * ((u - lin) / w).tanh()
    };
    y.copysign(x) * t
}

/// Topology-preserving state-variable filter (Simper). Stable for any coefficient update.
#[derive(Clone, Copy, Default)]
struct Svf {
    ic1: f32,
    ic2: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
}
impl Svf {
    fn set(&mut self, f: f32, sr: f32, k: f32) {
        let g = (std::f32::consts::PI * f.clamp(20.0, sr * 0.45) / sr).tan();
        self.k = k;
        self.a1 = 1.0 / (1.0 + g * (g + k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }
    /// (low, band, high)
    #[inline]
    fn tick(&mut self, v0: f32) -> (f32, f32, f32) {
        let v3 = v0 - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = flush(2.0 * v1 - self.ic1);
        self.ic2 = flush(2.0 * v2 - self.ic2);
        (v2, v1, v0 - self.k * v1 - v2)
    }
}

/// Schroeder all-pass on a power-of-two ring, fractional length.
#[derive(Default)]
struct Allpass {
    buf: Vec<f32>,
    w: usize,
}
impl Allpass {
    #[inline]
    fn tick(&mut self, x: f32, len: f32) -> f32 {
        let n = self.buf.len();
        if n < 4 {
            return x;
        }
        let m = n - 1;
        let pos = self.w as f32 + n as f32 - len.clamp(1.0, (n - 2) as f32);
        let i0 = pos as usize;
        let fr = pos - i0 as f32;
        let a = self.buf[i0 & m];
        let d = a + (self.buf[(i0 + 1) & m] - a) * fr;
        let v = x + AP_G * d;
        self.buf[self.w] = flush(v);
        self.w = (self.w + 1) & m;
        d - AP_G * v
    }
}

/// 4-point Hermite read `d` samples behind the write index `w` of a power-of-two ring.
#[inline]
fn read(buf: &[f32], w: usize, d: f32) -> f32 {
    let n = buf.len();
    let m = n - 1;
    let pos = w as f32 + n as f32 - d;
    let i = pos as usize;
    let f = pos - i as f32;
    let xm1 = buf[(i + n - 1) & m];
    let x0 = buf[i & m];
    let x1 = buf[(i + 1) & m];
    let x2 = buf[(i + 2) & m];
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * f + c2) * f + c1) * f + x0
}

/// One-pole glide toward a target.
#[derive(Clone, Copy, Default)]
struct Glide {
    v: f32,
    k: f32,
}
impl Glide {
    #[inline]
    fn next(&mut self, t: f32) -> f32 {
        self.v += (t - self.v) * self.k;
        self.v
    }
}

#[derive(Default)]
pub struct Delay {
    sr: f32,
    /// Raw parameter targets in catalog order.
    p: [f32; idx::COUNT],
    have_params: bool,
    ring: [Vec<f32>; 2],
    pre: [Vec<f32>; 2],
    w: usize,
    // smoothed continuous controls
    input: Glide,
    fb: Glide,
    wet: Glide,
    dry: Glide,
    spread: Glide,
    offset: Glide,
    pan: Glide,
    mtime: Glide,
    mcut: Glide,
    cutoff: Glide,
    th: Glide,
    diff: Glide,
    dspread: Glide,
    tone: Glide,
    // time
    /// Keep pitch off: the gliding delay in samples. Keep pitch on: the settled head.
    d_cur: f32,
    d_next: f32,
    xf: f32,
    fading: bool,
    slew_k: f32,
    // model switch dip
    model: usize,
    model_want: usize,
    sw: f32,
    // loop chain state
    svf: [Svf; 2],
    hold: [f32; 2],
    hold_ph: f32,
    dc: [(f32, f32); 2],
    ap: [[Allpass; 4]; 2],
    tone_f: [Biquad; 2],
    tone_on: bool,
    tone_last: f32,
    lfo: f64,
    ctl: usize,
    // meters
    m_d: [f32; 2],
    m_loop: f32,
    m_wet: f32,
    m_red: f32,
    m_lfo: f32,
    m_target: f32,
}

impl Delay {
    fn model_param(&self) -> usize {
        (self.p[idx::MODEL].round() as usize).min(OFF)
    }

    fn glide_k(&self, ms: f32) -> f32 {
        1.0 - (-1.0 / (self.sr * ms / 1000.0)).exp()
    }

    fn snap_smoothers(&mut self) {
        let p = self.p;
        self.input.v = p[idx::INPUT] / 100.0;
        self.fb.v = p[idx::FB] / 100.0;
        self.wet.v = p[idx::MIX] / 100.0;
        self.dry.v = p[idx::DRY] / 100.0;
        self.spread.v = p[idx::SPREAD] / 100.0;
        self.offset.v = p[idx::OFFSET] / 100.0;
        self.pan.v = p[idx::PAN] / 100.0;
        self.mtime.v = p[idx::MTIME] / 100.0;
        self.mcut.v = p[idx::MCUT] / 100.0;
        self.cutoff.v = p[idx::CUTOFF].max(20.0).ln();
        self.th.v = super::db_to_gain(p[idx::LEVEL]);
        self.diff.v = p[idx::DIFF] / 100.0;
        self.dspread.v = p[idx::DSPREAD] / 100.0;
        self.tone.v = p[idx::TONE];
        self.model = self.model_param();
        self.model_want = self.model;
        self.sw = 1.0;
    }

    /// Loop gain bound used for the tail estimate (None = may sustain forever).
    fn loop_gain(&self) -> Option<f32> {
        let fb = self.p[idx::FB] / 100.0;
        let ft = self.p[idx::FTYPE].round() as usize;
        let peak = if ft == 0 {
            1.0
        } else {
            (1.0 / res_k(self.p[idx::RES])).max(1.0)
        };
        let g = fb * peak;
        if g >= 0.999 {
            None
        } else {
            Some(g)
        }
    }

    /// The loop chain for channel `c`. `tick`: the lo-fi clock fired this sample (the held
    /// value is refreshed); with sample-rate reduction off it fires every sample.
    // the loop chain takes its per-sample state as arguments so it stays inlined in the hot loop
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn chain(
        &mut self,
        c: usize,
        x: f32,
        ft: usize,
        tick: bool,
        q: f32,
        dist: usize,
        th: f32,
        knee: f32,
        sym: f32,
        diff: f32,
        ap_len: &[f32; 4],
    ) -> f32 {
        let mut v = x;
        if ft != 0 {
            let (lo, bp, hi) = self.svf[c].tick(v);
            v = match ft {
                1 => lo,
                2 => hi,
                _ => bp * std::f32::consts::SQRT_2,
            };
        }
        if tick {
            self.hold[c] = v;
        }
        v = self.hold[c];
        let s = shape(v, dist, th, knee, sym);
        self.m_red = self.m_red.max(v.abs() - s.abs());
        // DC blocker (≈ 5 Hz): the asymmetric curve must not pile up an offset in the loop
        let (x1, y1) = self.dc[c];
        let y = flush(s - x1 + 0.9993 * y1);
        self.dc[c] = (s, y);
        let mut v = y;
        if q > 0.0 {
            v = (v * q).round() / q;
        }
        if diff > 1e-4 {
            let mut a = v;
            for (k, ap) in self.ap[c].iter_mut().enumerate() {
                a = ap.tick(a, ap_len[k]);
            }
            v += (a - v) * diff;
        }
        v
    }
}

impl Processor for Delay {
    fn prepare(&mut self, sr: f32, _max_block: usize) {
        self.sr = sr;
        let n = ((MAX_DELAY_S + 2.0 * MOD_S) * sr + 8.0) as usize;
        let n = n.next_power_of_two();
        self.ring = [vec![0.0; n], vec![0.0; n]];
        self.pre = [vec![0.0; n], vec![0.0; n]];
        self.w = 0;
        let apn = ((AP_MS[3] * 2.2 * 1.07 / 1000.0) * sr + 8.0) as usize;
        for ch in self.ap.iter_mut() {
            for ap in ch.iter_mut() {
                ap.buf = vec![0.0; apn.next_power_of_two()];
                ap.w = 0;
            }
        }
        let k = self.glide_k(20.0);
        for g in [
            &mut self.input,
            &mut self.fb,
            &mut self.wet,
            &mut self.dry,
            &mut self.spread,
            &mut self.offset,
            &mut self.pan,
            &mut self.mtime,
            &mut self.mcut,
            &mut self.cutoff,
            &mut self.th,
            &mut self.diff,
            &mut self.dspread,
            &mut self.tone,
        ] {
            g.k = k;
        }
        // slow controls that move the read head glide longer (no zipper on the echoes)
        self.offset.k = self.glide_k(80.0);
        self.dspread.k = self.glide_k(80.0);
        if self.have_params {
            self.snap_smoothers();
        }
        self.tone_last = f32::NAN;
        self.d_cur = 0.0;
    }

    fn set_params(&mut self, pb: &ParamBlock) {
        let first = !self.have_params;
        self.p.copy_from_slice(&pb.values[..idx::COUNT]);
        self.have_params = true;
        let s = (self.p[idx::SMOOTH] / 100.0).clamp(0.0, 1.0);
        // 0 % = jump, 50 % ≈ 55 ms, 100 % = 1.5 s time constant
        self.slew_k = if s <= 0.0 || self.sr <= 0.0 {
            1.0
        } else {
            self.glide_k(2.0 * 750f32.powf(s))
        };
        self.model_want = self.model_param();
        if first && self.sr > 0.0 {
            self.snap_smoothers();
        }
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &ProcessCtx) {
        let n = self.ring[0].len();
        if n == 0 || !self.have_params {
            return;
        }
        let sr = self.sr;
        let p = self.p;
        let target_s = delay_seconds(p[idx::SYNC], p[idx::BEATS], p[idx::TIME], ctx.bpm);
        self.m_target = target_s * 1000.0;
        let max_d = MAX_DELAY_S * sr;
        let target = (target_s * sr).clamp(2.0, max_d);
        if self.d_cur < 1.0 {
            self.d_cur = target;
            self.d_next = target;
        }
        let keep = p[idx::KEEP] >= 0.5;
        let ft = (p[idx::FTYPE].round() as usize).min(3);
        let k_res = res_k(p[idx::RES]);
        let srate = p[idx::SRATE];
        let lofi_sr = srate < SRATE_OFF && srate < sr;
        let bits = p[idx::BITS];
        let q = if bits < BITS_OFF {
            2f32.powf(bits.round().max(1.0) - 1.0)
        } else {
            0.0
        };
        let dist = if p[idx::DIST] >= 0.5 { 1 } else { 0 };
        let knee = (p[idx::KNEE] / 100.0).clamp(0.0, 1.0);
        let sym = (p[idx::SYM] / 100.0).clamp(0.0, 1.0);
        let rate = p[idx::RATE].max(0.0) as f64;
        let lfo_step = rate / sr as f64;
        let xf_step = 1.0 / (XFADE_S * sr);
        let sw_step = 1.0 / (SWITCH_S * sr);
        let ftype_lp = ft;
        let mask = n - 1;
        let sc = 0.3 + 1.9 * self.dspread.v;
        let mut ap_len = AP_MS.map(|ms| ms * sc * sr / 1000.0);

        for i in 0..l.len() {
            // control rate: filter, tone, all-pass lengths (every 16 samples)
            if self.ctl == 0 {
                let lfo_v = (self.lfo * std::f64::consts::TAU).sin() as f32;
                if ftype_lp != 0 {
                    let f = self.cutoff.v.exp() * 2f32.powf(3.0 * self.mcut.v * lfo_v);
                    for c in 0..2 {
                        self.svf[c].set(f, sr, k_res);
                    }
                }
                let tv = self.tone.v;
                if (tv - self.tone_last).abs() > 0.05 || self.tone_last.is_nan() {
                    self.tone_last = tv;
                    match tone_filter(tv) {
                        Some((t, f)) => {
                            self.tone_on = true;
                            for b in self.tone_f.iter_mut() {
                                b.set(t, sr, f, std::f32::consts::FRAC_1_SQRT_2, 0.0);
                            }
                        }
                        None => self.tone_on = false,
                    }
                }
                let sc = 0.3 + 1.9 * self.dspread.v;
                for (k, len) in ap_len.iter_mut().enumerate() {
                    *len = AP_MS[k] * sc * sr / 1000.0;
                }
            }
            self.ctl = (self.ctl + 1) & 15;

            // smoothed controls
            let ing = self.input.next(p[idx::INPUT] / 100.0);
            let fb = self.fb.next(p[idx::FB] / 100.0);
            let wet = self.wet.next(p[idx::MIX] / 100.0);
            let dry = self.dry.next(p[idx::DRY] / 100.0);
            let spread = self.spread.next(p[idx::SPREAD] / 100.0);
            let off = self.offset.next(p[idx::OFFSET] / 100.0);
            let pan = self.pan.next(p[idx::PAN] / 100.0);
            let mtime = self.mtime.next(p[idx::MTIME] / 100.0);
            self.mcut.next(p[idx::MCUT] / 100.0);
            self.cutoff.next(p[idx::CUTOFF].max(20.0).ln());
            let th = self.th.next(super::db_to_gain(p[idx::LEVEL]));
            let diff = self.diff.next(p[idx::DIFF] / 100.0);
            self.dspread.next(p[idx::DSPREAD] / 100.0);
            self.tone.next(p[idx::TONE]);

            // LFO
            let lfo_v = (self.lfo * std::f64::consts::TAU).sin() as f32;
            self.lfo += lfo_step;
            if self.lfo >= 1.0 {
                self.lfo -= 1.0;
            }

            // model switch: dip the wet path, switch at the bottom
            if self.model_want != self.model {
                self.sw -= sw_step;
                if self.sw <= 0.0 {
                    self.sw = 0.0;
                    self.model = self.model_want;
                }
            } else if self.sw < 1.0 {
                self.sw = (self.sw + sw_step).min(1.0);
            }
            let model = self.model;

            // time: glide (repitch) or two-head crossfade (keep pitch)
            let (da, db, g_b) = if keep {
                if !self.fading && (target - self.d_cur).abs() > 0.5 {
                    self.fading = true;
                    self.d_next = target;
                    self.xf = 0.0;
                }
                if self.fading {
                    self.xf += xf_step;
                    if self.xf >= 1.0 {
                        self.d_cur = self.d_next;
                        self.fading = false;
                        self.xf = 0.0;
                    }
                }
                (
                    self.d_cur,
                    self.d_next,
                    if self.fading { self.xf } else { 0.0 },
                )
            } else {
                self.fading = false;
                self.d_cur += (target - self.d_cur) * self.slew_k;
                (self.d_cur, self.d_cur, 0.0)
            };
            let modv = mtime * lfo_v * (MOD_S * sr).min(0.45 * da);
            // equal-power crossfade between the heads
            let (ga, gb) = if g_b > 0.0 {
                let a = g_b * std::f32::consts::FRAC_PI_2;
                (a.cos(), a.sin())
            } else {
                (1.0, 0.0)
            };

            // lo-fi hold clock (shared by both channels)
            let mut tick = true;
            if lofi_sr {
                self.hold_ph += srate / sr;
                if self.hold_ph >= 1.0 {
                    self.hold_ph -= 1.0;
                } else {
                    tick = false;
                }
            }

            let (xl, xr) = (l[i], r[i]);
            let (mut wl, mut wr);
            if model == OFF {
                let (a, b) = (xl * ing, xr * ing);
                wl = self.chain(0, a, ft, tick, q, dist, th, knee, sym, diff, &ap_len);
                wr = self.chain(1, b, ft, tick, q, dist, th, knee, sym, diff, &ap_len);
                self.m_d = [0.0, 0.0];
            } else {
                // per-channel loop multipliers and pre-delays
                let (ml, mr, pl, pr) = match model {
                    MONO => (1.0 + (-off).max(0.0), 1.0 + off.max(0.0), 0.0, 0.0),
                    STEREO => (1.0, 1.0, (-off).max(0.0), off.max(0.0)),
                    _ => (1.0, 1.0, 0.0, 0.0),
                };
                let lim = max_d;
                let dl = |d: f32, m: f32| (d * m + modv).clamp(2.0, lim);
                let (dla, dra) = (dl(da, ml), dl(da, mr));
                let mut yl = read(&self.ring[0], self.w, dla) * ga;
                let mut yr = read(&self.ring[1], self.w, dra) * ga;
                if gb > 0.0 {
                    yl += read(&self.ring[0], self.w, dl(db, ml)) * gb;
                    yr += read(&self.ring[1], self.w, dl(db, mr)) * gb;
                }
                self.m_d = [dla / sr * 1000.0, dra / sr * 1000.0];
                let zl = self.chain(0, yl, ft, tick, q, dist, th, knee, sym, diff, &ap_len);
                let zr = self.chain(1, yr, ft, tick, q, dist, th, knee, sym, diff, &ap_len);
                // input into the lines
                let (il, ir) = match model {
                    MONO => {
                        let s = (xl + xr) * 0.5 * ing;
                        (s, s)
                    }
                    PINGPONG => {
                        // balance pan on the input (like the mixer), then crossed into the lines
                        let gl = if pan > 0.0 { 1.0 - pan } else { 1.0 };
                        let gr = if pan < 0.0 { 1.0 + pan } else { 1.0 };
                        (xl * gl * ing, xr * gr * ing)
                    }
                    _ => {
                        let (a, b) = (xl * ing, xr * ing);
                        self.pre[0][self.w] = a;
                        self.pre[1][self.w] = b;
                        let pa = pl * da;
                        let pbv = pr * da;
                        (
                            if pa >= 1.0 {
                                read(&self.pre[0], self.w, pa.min(lim))
                            } else {
                                a
                            },
                            if pbv >= 1.0 {
                                read(&self.pre[1], self.w, pbv.min(lim))
                            } else {
                                b
                            },
                        )
                    }
                };
                let (nl, nr) = if model == PINGPONG {
                    (ir + fb * zr, il + fb * zl)
                } else {
                    (il + fb * zl, ir + fb * zr)
                };
                let guard = |v: f32| {
                    if v.is_finite() {
                        flush(v.clamp(-SAFETY, SAFETY))
                    } else {
                        0.0
                    }
                };
                self.ring[0][self.w] = guard(nl);
                self.ring[1][self.w] = guard(nr);
                wl = zl;
                wr = zr;
            }
            self.w = (self.w + 1) & mask;
            if !wl.is_finite() {
                wl = 0.0;
            }
            if !wr.is_finite() {
                wr = 0.0;
            }
            self.m_loop = self.m_loop.max(wl.abs().max(wr.abs()));
            // spread (mono ← → stereo) and tone, then the output mix
            let mid = 0.5 * (wl + wr);
            let side = 0.5 * (wl - wr) * spread;
            let (mut ol, mut or) = (mid + side, mid - side);
            if self.tone_on {
                ol = self.tone_f[0].tick(ol);
                or = self.tone_f[1].tick(or);
            }
            let g = wet * self.sw;
            ol *= g;
            or *= g;
            self.m_wet = self.m_wet.max(ol.abs().max(or.abs()));
            l[i] = xl * dry + ol;
            r[i] = xr * dry + or;
            self.m_lfo = lfo_v;
        }
        // meter peaks fall ~20 dB/s
        let fall = (-(l.len() as f32) / (0.12 * sr)).exp();
        self.m_loop *= fall;
        self.m_wet *= fall;
        self.m_red *= fall;
    }

    fn reset(&mut self) {
        for b in self.ring.iter_mut().chain(self.pre.iter_mut()) {
            b.fill(0.0);
        }
        for ch in self.ap.iter_mut() {
            for ap in ch.iter_mut() {
                ap.buf.fill(0.0);
            }
        }
        self.svf = Default::default();
        self.dc = Default::default();
        self.hold = [0.0; 2];
        for b in self.tone_f.iter_mut() {
            b.reset();
        }
        self.fading = false;
        if self.have_params {
            self.snap_smoothers();
        }
        self.d_cur = 0.0;
        self.m_loop = 0.0;
        self.m_wet = 0.0;
        self.m_red = 0.0;
    }

    fn tail_samples(&self) -> Option<usize> {
        if self.ring[0].is_empty() || !self.have_params {
            return Some(0);
        }
        let sr = self.sr;
        // ringing of the loop filter and the diffusion, plus the tone filter
        let settle = (0.5 * sr) as usize;
        if self.model_param() == OFF && self.model == OFF {
            return Some(settle);
        }
        let g = self.loop_gain()?;
        let d = (self.d_cur.max(self.d_next) * 2.0 + MOD_S * sr).min(MAX_DELAY_S * sr * 2.0);
        let reps = if g < 1e-4 {
            1.0
        } else {
            (16.2 / -g.ln()).ceil().max(1.0)
        };
        Some(((reps + 1.0) * d) as usize + settle)
    }

    fn park(&mut self) {
        self.reset();
    }

    fn meters(&self, out: &mut [f32; METERS]) {
        let db = |x: f32| if x > 1e-6 { 20.0 * x.log10() } else { -120.0 };
        out[0] = self.m_d[0];
        out[1] = self.m_d[1];
        out[2] = db(self.m_loop);
        out[3] = if self.m_red > 1e-6 {
            -db(1.0 + self.m_red)
        } else {
            0.0
        };
        out[4] = self.m_lfo;
        out[5] = self.model as f32;
        out[6] = db(self.m_wet);
        out[7] = self.m_target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(vals: &[(usize, f32)]) -> ParamBlock {
        let mut pb = ParamBlock::default();
        let defs = crate::catalog::params(crate::model::FxKind::Delay);
        for (i, d) in defs.iter().enumerate() {
            pb.values[i] = d.def as f32;
        }
        for &(i, v) in vals {
            pb.values[i] = v;
        }
        pb
    }

    fn run(
        pb: ParamBlock,
        mut input: impl FnMut(usize) -> (f32, f32),
        n: usize,
        bpm: f64,
    ) -> (Vec<f32>, Vec<f32>, Delay) {
        let mut d = Delay::default();
        d.prepare(48000.0, 256);
        d.set_params(&pb);
        let ctx = ProcessCtx {
            sample_rate: 48000.0,
            bpm,
            beat: 0.0,
            playing: true,
        };
        let (mut ol, mut or) = (vec![], vec![]);
        let mut i = 0;
        while i < n {
            let m = 256.min(n - i);
            let (mut l, mut r): (Vec<f32>, Vec<f32>) = (i..i + m).map(&mut input).unzip();
            d.process(&mut l, &mut r, &ctx);
            ol.extend_from_slice(&l);
            or.extend_from_slice(&r);
            i += m;
        }
        (ol, or, d)
    }

    fn peak_at(x: &[f32]) -> usize {
        x.iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, _)| i)
            .unwrap()
    }

    /// plain echo: no filter, clean, no dry
    fn clean(extra: &[(usize, f32)]) -> ParamBlock {
        let mut v = vec![
            (idx::FTYPE, 0.0),
            (idx::DRY, 0.0),
            (idx::MIX, 100.0),
            (idx::FB, 0.0),
            (idx::LEVEL, 0.0),
            (idx::DIST, 0.0),
        ];
        v.extend_from_slice(extra);
        block(&v)
    }

    #[test]
    fn synced_echo_lands_on_the_beat_and_follows_tempo() {
        // 1/2 beat at 120 bpm = 250 ms = 12 000 samples
        let pb = clean(&[(idx::SYNC, 0.0), (idx::BEATS, 0.5)]);
        let (l, _, _) = run(
            pb,
            |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) },
            24000,
            120.0,
        );
        assert_eq!(peak_at(&l), 12000);
        let (l, _, _) = run(
            pb,
            |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) },
            24000,
            100.0,
        );
        assert_eq!(peak_at(&l), 14400, "half a beat at 100 bpm = 300 ms");
    }

    #[test]
    fn free_time_in_ms() {
        let pb = clean(&[(idx::SYNC, 1.0), (idx::TIME, 100.0)]);
        let (l, _, _) = run(
            pb,
            |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) },
            9600,
            120.0,
        );
        assert_eq!(peak_at(&l), 4800);
        assert!((l[4800] - 1.0).abs() < 0.02, "unity echo, got {}", l[4800]);
    }

    #[test]
    fn ping_pong_alternates_sides() {
        let pb = clean(&[
            (idx::SYNC, 1.0),
            (idx::TIME, 50.0),
            (idx::MODEL, PINGPONG as f32),
            (idx::FB, 80.0),
            (idx::PAN, 0.0),
        ]);
        // input hard left
        let (l, r, _) = run(
            pb,
            |i| if i == 0 { (1.0, 0.0) } else { (0.0, 0.0) },
            9600,
            120.0,
        );
        let d = 2400;
        assert!(
            r[d].abs() > 0.5 && l[d].abs() < 1e-3,
            "first echo on the right: l {} r {}",
            l[d],
            r[d]
        );
        assert!(
            l[2 * d].abs() > 0.3 && r[2 * d].abs() < 1e-3,
            "second echo on the left"
        );
        assert!(
            r[3 * d].abs() > 0.2 && l[3 * d].abs() < 1e-3,
            "third on the right"
        );
    }

    #[test]
    fn stereo_offset_delays_one_side() {
        let pb = clean(&[
            (idx::SYNC, 1.0),
            (idx::TIME, 100.0),
            (idx::MODEL, STEREO as f32),
            (idx::OFFSET, 50.0),
        ]);
        let (l, r, _) = run(
            pb,
            |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) },
            19200,
            120.0,
        );
        assert_eq!(peak_at(&l), 4800);
        assert_eq!(
            peak_at(&r),
            7200,
            "right gets a pre-delay of 50 % of the time"
        );
    }

    #[test]
    fn runaway_feedback_stays_bounded_and_finite() {
        // 125 % feedback, resonant band-pass, hard input: must neither blow up nor NaN
        for dist in [0.0, 1.0] {
            let pb = block(&[
                (idx::SYNC, 1.0),
                (idx::TIME, 30.0),
                (idx::FB, 125.0),
                (idx::FTYPE, 3.0),
                (idx::RES, 100.0),
                (idx::CUTOFF, 900.0),
                (idx::DIST, dist),
                (idx::LEVEL, 0.0),
                (idx::MIX, 100.0),
                (idx::DRY, 0.0),
                (idx::SYM, 100.0),
            ]);
            let mut seed = 1u32;
            let (l, r, _) = run(
                pb,
                move |i| {
                    seed = seed
                        .wrapping_mul(1664525)
                        .wrapping_add(1013904223 ^ i as u32);
                    let x = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
                    (x, -x)
                },
                48000 * 20,
                120.0,
            );
            let peak = l.iter().chain(r.iter()).fold(0f32, |m, x| m.max(x.abs()));
            assert!(
                l.iter().chain(r.iter()).all(|x| x.is_finite()),
                "NaN in the output"
            );
            assert!(peak <= SAFETY + 1e-3, "loop escaped: {peak}");
        }
    }

    #[test]
    fn self_oscillation_sustains_above_unity() {
        // an impulse into 110 % feedback with saturation keeps ringing after 10 s
        let pb = block(&[
            (idx::SYNC, 1.0),
            (idx::TIME, 100.0),
            (idx::FB, 110.0),
            (idx::FTYPE, 0.0),
            (idx::DIST, 1.0),
            (idx::LEVEL, -6.0),
            (idx::MIX, 100.0),
            (idx::DRY, 0.0),
        ]);
        let (l, _, d) = run(
            pb,
            |i| if i < 48 { (0.8, 0.8) } else { (0.0, 0.0) },
            48000 * 10,
            120.0,
        );
        let late = l[l.len() - 48000..]
            .iter()
            .fold(0f32, |m, x| m.max(x.abs()));
        assert!(late > 0.2, "self-oscillation died: {late}");
        assert!(
            d.tail_samples().is_none(),
            "a sustaining loop is never parked"
        );
    }

    #[test]
    fn keep_pitch_crossfades_instead_of_gliding() {
        // a sine through the delay while the time jumps: with keep pitch the output frequency
        // never leaves the input's (no Doppler), checked by zero-crossing intervals
        let f0 = 1000.0f32;
        let sine = move |i: usize| {
            let v = (i as f32 * f0 * std::f32::consts::TAU / 48000.0).sin() * 0.5;
            (v, v)
        };
        for (keep, expect_bend) in [(1.0, false), (0.0, true)] {
            let mut d = Delay::default();
            d.prepare(48000.0, 256);
            d.set_params(&clean(&[
                (idx::SYNC, 1.0),
                (idx::TIME, 200.0),
                (idx::KEEP, keep),
                (idx::SMOOTH, 60.0),
            ]));
            let ctx = ProcessCtx {
                sample_rate: 48000.0,
                bpm: 120.0,
                beat: 0.0,
                playing: true,
            };
            let mut out = vec![];
            for b in 0..(48000 * 2 / 256) {
                if b == 48000 / 256 {
                    d.set_params(&clean(&[
                        (idx::SYNC, 1.0),
                        (idx::TIME, 300.0),
                        (idx::KEEP, keep),
                        (idx::SMOOTH, 60.0),
                    ]));
                }
                let (mut l, mut r): (Vec<f32>, Vec<f32>) =
                    (b * 256..b * 256 + 256).map(sine).unzip();
                d.process(&mut l, &mut r, &ctx);
                out.extend_from_slice(&l);
            }
            // longest deviation of a half period from 24 samples in the 0.3 s after the change
            let seg = &out[48000..48000 + 14400];
            let mut last: Option<f32> = None;
            let mut worst = 0f32;
            for i in 1..seg.len() {
                if seg[i - 1] < 0.0 && seg[i] >= 0.0 {
                    let x = i as f32 - seg[i] / (seg[i] - seg[i - 1]);
                    if let Some(p) = last {
                        worst = worst.max(((x - p) - 48.0f32).abs());
                    }
                    last = Some(x);
                }
            }
            if expect_bend {
                assert!(
                    worst > 2.0,
                    "repitch mode should bend the pitch (worst {worst})"
                );
            } else {
                assert!(
                    worst < 0.6,
                    "keep pitch bent the pitch: period off by {worst} samples"
                );
            }
        }
    }

    #[test]
    fn model_off_is_an_instant_processor() {
        // no ring: a click comes out at once, through the chain
        let pb = clean(&[(idx::MODEL, OFF as f32)]);
        let (l, _, _) = run(
            pb,
            |i| if i == 100 { (0.5, 0.5) } else { (0.0, 0.0) },
            4800,
            120.0,
        );
        assert_eq!(peak_at(&l), 100);
    }

    #[test]
    fn lofi_reduces_bits() {
        let pb = clean(&[(idx::MODEL, OFF as f32), (idx::BITS, 3.0)]);
        let (l, _, _) = run(pb, |i| ((i as f32 * 0.001).sin() * 0.9, 0.0), 4800, 120.0);
        let mut levels: Vec<i32> = l[1000..]
            .iter()
            .map(|x| (x * 1000.0).round() as i32)
            .collect();
        levels.sort();
        levels.dedup();
        assert!(
            levels.len() <= 12,
            "3 bits leave few levels, got {}",
            levels.len()
        );
    }

    #[test]
    fn shape_is_bounded_and_linear_below_the_knee() {
        for knee in [0.0, 0.5, 1.0] {
            for i in -400..=400 {
                let x = i as f32 / 20.0;
                let y = shape(x, 1, 0.5, knee, 0.0);
                assert!(y.abs() <= 0.5 + 1e-6);
            }
        }
        assert!(
            (shape(0.2, 1, 1.0, 0.5, 0.0) - 0.2).abs() < 1e-6,
            "below the knee the curve is the identity"
        );
        assert_eq!(shape(2.0, 0, 0.5, 0.0, 0.0), 0.5);
    }

    #[test]
    fn tail_is_finite_and_covers_the_echoes() {
        let pb = clean(&[(idx::SYNC, 1.0), (idx::TIME, 250.0), (idx::FB, 50.0)]);
        let (_, _, d) = run(pb, |_| (0.0, 0.0), 2048, 120.0);
        let t = d.tail_samples().unwrap();
        assert!(
            t > 12000 * 20,
            "50 % feedback needs ~24 repeats to fall 140 dB, got {t}"
        );
    }

    #[test]
    fn first_delay_migrates_to_the_same_echo() {
        // an old project: 300 ms, 50 % feedback, 40 % mix
        let mut p: std::collections::BTreeMap<String, f64> =
            [("time", 300.0), ("fb", 50.0), ("mix", 40.0)]
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect();
        assert!(migrate_params(&mut p));
        assert!(!migrate_params(&mut p), "idempotent");
        let fx = crate::model::Fx {
            id: 1,
            kind: crate::model::FxKind::Delay,
            bypass: false,
            mix: 1.0,
            preset: None,
            params: p,
            bands: None,
            plugin: None,
            maps: None,
        };
        let (l, _, _) = run(
            crate::catalog::param_block(&fx),
            |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) },
            48000,
            140.0,
        );
        let d = 300 * 48;
        assert!((l[0] - 1.0).abs() < 1e-3, "dry passes at unity");
        assert!((l[d] - 0.4).abs() < 0.01, "first echo = mix: {}", l[d]);
        assert!(
            (l[2 * d] - 0.2).abs() < 0.01,
            "second echo = mix × fb: {}",
            l[2 * d]
        );
        // automation of the old time range lands on the same ms
        let v_old = (300f32 / 10.0).ln() / 150f32.ln();
        let v_new = migrate_auto_value("time", v_old);
        assert!((2000f32.powf(v_new) - 300.0).abs() < 0.5);
    }

    #[test]
    fn ping_pong_preset_bounces_a_centred_mono_source() {
        // a vocal in the middle of the stereo field: the preset's input pan starts the bounce
        let list = crate::catalog::presets(crate::model::FxKind::Delay);
        let i = list.iter().position(|p| p.name == "Ping-pong 1/8").unwrap();
        let mut fx = crate::catalog::new_fx(1, crate::model::FxKind::Delay, 120.0);
        crate::catalog::load_preset(&mut fx, i, 120.0);
        fx.params.insert("dry".into(), 0.0);
        fx.params.insert("ftype".into(), 0.0);
        let (l, r, _) = run(
            crate::catalog::param_block(&fx),
            |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) },
            60000,
            120.0,
        );
        let d = 12000; // 1/2 beat at 120 bpm
        let side =
            |x: &[f32], at: usize| x[at - 4..at + 4].iter().fold(0f32, |m, v| m.max(v.abs()));
        for k in 1..=4 {
            let (a, b) = (side(&l, k * d), side(&r, k * d));
            if k % 2 == 1 {
                assert!(b > 2.0 * a, "echo {k} should be on the right: L {a} R {b}");
            } else {
                assert!(a > 2.0 * b, "echo {k} should be on the left: L {a} R {b}");
            }
        }
    }
}
