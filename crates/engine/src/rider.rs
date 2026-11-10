//! Vocal rider: evens out words toward the level of their phrase, before any compressor.
//!
//! Port of the auto-vocal research rider (`research/auto_vocal/av/mix.py::rider_curve`,
//! branch `auto-vocal`), validated there on 50 degraded MUSDB songs and 40 real Vocadito
//! takes. Every 10 ms, on K-weighted (ITU-R BS.1770) power:
//!
//! * `L_word`: level over ~200 ms (sung) / ~110 ms (rap), centred on the frame;
//! * `L_phrase`: level over 2 s / 1 s, averaged over confident vocal frames only;
//! * desired gain = `amount · deadband(L_phrase − L_word, ±0.6 dB) + phrase · (T − L_phrase)`,
//!   with `T` the take's typical sung level (`target`). Words move toward their phrase;
//!   phrases are only nudged toward the take, so verse/chorus dynamics survive.
//! * Vocal confidence (`smoothstep(SNR 6 → 14 dB over the noise floor)` × activity) can
//!   block a *boost* but never a *cut*, and idle frames glide back to 0 dB: breaths and
//!   room noise are not lifted.
//! * Range `+boost / −cut`; asymmetric slew (80 ms toward cut, 300 ms toward boost), run
//!   forwards and — over the look-ahead — backwards, and averaged, as the offline version.
//!
//! Patent note (kept from the research): this is a local-vs-phrase error with
//! confidence-limited positive gain. It deliberately avoids the "activity detector →
//! long-term level → target gain → direction-dependent ramp" AGC structure claimed in
//! US7995775B2. Keep it that way.
//!
//! Real time: the analysis needs future frames, so the audio is delayed by a look-ahead
//! reported to the host through [`latency_samples`] (plugin delay compensation). "Long"
//! look-ahead reproduces the offline windows exactly (phrase window centred, 0.5 s of
//! backward slew, ≈ 1.6 s latency); "Short" uses a trailing phrase window and 0.2 s of
//! backward slew (≈ 0.3 s latency). "Live" (the default) adds no latency at all: word and
//! phrase windows trail the audio, the gain is slewed forwards only and applied
//! [`LIVE_LAG_HOPS`] frames behind the analysis (a 20 ms lag of a curve that slews over
//! 30–100 ms), so the mix is never delayed. It reacts later than the look-ahead modes and
//! evens words less (≈ 3 dB of a 10 dB spread on the test phrase).
//!
//! Meters: `[0]` gain applied dB, `[1]` word level dB, `[2]` phrase level dB,
//! `[3]` vocal confidence 0..1 (all at the moment being heard).

use crate::dsp::{Biquad, FilterType};
use crate::processor::{ParamBlock, ProcessCtx, Processor, METERS};

/// Parameter slots, in the order of `catalog::params(FxKind::Rider)`.
pub mod idx {
    pub const AMOUNT: usize = 0;
    pub const PHRASE: usize = 1;
    pub const UP: usize = 2;
    pub const DOWN: usize = 3;
    pub const SPEED: usize = 4;
    pub const AUTO: usize = 5;
    pub const TARGET: usize = 6;
    pub const FLOOR: usize = 7;
    pub const LOOK: usize = 8;
    pub const OUT: usize = 9;
    pub const GATE: usize = 10;
}

/// Frame ring capacity (power of two, well over the longest window + look-ahead).
const RING: usize = 1024;
const MASK: usize = RING - 1;
const DEADBAND_DB: f32 = 0.6;
const CUT_MS: f32 = 80.0;
const BOOST_MS: f32 = 300.0;
/// Live mode slews (no backward pass to soften them; tuned on `live_mode_evens_words_without_delay`).
const LIVE_CUT_MS: f32 = 30.0;
const LIVE_BOOST_MS: f32 = 100.0;
/// Frames whose word level is below this are digital silence: auto estimates ignore them.
const SILENCE_DB: f32 = -100.0;
const P_MIN: f64 = 1e-14;

/// Look-ahead modes (`look` parameter values).
pub mod look {
    pub const SHORT: usize = 0;
    pub const LONG: usize = 1;
    pub const LIVE: usize = 2;
}

/// Live mode: the gain heard at sample `s` is the curve at `s − LIVE_LAG_HOPS · hop`, the
/// latest point whose interpolation frames are always analysed (needs > 1.5 hops).
pub const LIVE_LAG_HOPS: usize = 2;

/// The look-ahead mode of a `look` parameter value (old projects: 0 short, 1 long).
pub fn look_mode(look: f32) -> usize {
    if look >= 1.5 {
        look::LIVE
    } else if look >= 0.5 {
        look::LONG
    } else {
        look::SHORT
    }
}

/// Analysis hop: 10 ms (`int(0.01 · sr)` as in the research code).
pub fn hop(sr: f32) -> usize {
    ((sr as f64 * 0.01) as usize).max(1)
}

/// (word window frames, phrase window frames) per speed: 0 = sung, 1 = rap.
/// The research code takes `int(seconds / 0.01)`, which gives 20/200 and 11/100.
fn windows(speed: usize) -> (usize, usize) {
    if speed == 1 {
        (11, 100)
    } else {
        (20, 200)
    }
}

/// Frames the analysis looks ahead: (word window ahead, phrase window ahead, backward slew).
fn ahead(speed: usize, mode: usize) -> (usize, usize, usize) {
    let (wf, wp) = windows(speed);
    let fa = wf - wf / 2 - 1;
    match mode {
        look::LONG => (fa, wp - wp / 2 - 1, 50),
        look::SHORT => (fa, 0, 20),
        _ => (0, 0, 0),
    }
}

/// Delay the rider adds, in samples, for `speed` (0 sung / 1 rap) and look-ahead (0 short /
/// 1 long / 2 live: none).
pub fn latency_samples(speed: f32, look: f32, sr: f32) -> usize {
    let mode = look_mode(look);
    if mode == look::LIVE {
        return 0;
    }
    let (fa, ap, b) = ahead(speed.round().clamp(0.0, 1.0) as usize, mode);
    (fa + ap + b + 2) * hop(sr)
}

/// ITU-R BS.1770 K-weighting (high shelf +4 dB at 1.5 kHz, then a 38 Hz high-pass),
/// with the RBJ coefficients pyloudnorm uses, so levels match the research measurements.
#[derive(Clone, Copy, Default)]
pub struct KWeight {
    shelf: Biquad,
    hp: Biquad,
}

impl KWeight {
    pub fn new(sr: f32) -> Self {
        let mut k = Self::default();
        k.shelf.set(FilterType::HighShelf, sr, 1500.0, std::f32::consts::FRAC_1_SQRT_2, 4.0);
        k.hp.set(FilterType::Highpass, sr, 38.0, 0.5, 0.0);
        k
    }
    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        self.hp.tick(self.shelf.tick(x))
    }
    pub fn reset(&mut self) {
        self.shelf.reset();
        self.hp.reset();
    }
}

#[derive(Clone, Copy, Default)]
struct Frame {
    /// mean K-weighted power of the frame
    pk: f64,
    /// word-window power and level
    pf: f64,
    lf: f32,
    conf: f32,
    active: bool,
    lp: f32,
    /// desired gain, forward-slewed gain, final gain (dB)
    g: f32,
    fw: f32,
    gs: f32,
}

/// Running estimates for "Auto" target (no analysis available): a slow peak follower for
/// the loud end, a floor follower, and the typical sung level. They only move on frames
/// that are not digital silence, so silence skipping leaves them untouched.
#[derive(Clone, Copy)]
struct AutoEst {
    top: f32,
    floor: f32,
    target: f32,
    seeded: bool,
}

impl Default for AutoEst {
    fn default() -> Self {
        Self { top: -30.0, floor: -80.0, target: -36.0, seeded: false }
    }
}

pub struct Rider {
    sr: f32,
    hop: usize,
    kl: KWeight,
    kr: KWeight,
    // parameters
    amount: f32,
    phrase: f32,
    up: f32,
    down: f32,
    speed: usize,
    auto: bool,
    target: f32,
    floor: f32,
    gate: f32,
    mode: usize,
    out: f32,
    wf: usize,
    wp: usize,
    fa: usize,
    ap: usize,
    back: usize,
    a_cut: f32,
    a_boost: f32,
    latency: usize,
    // state
    frames: Vec<Frame>,
    /// frames completed since the canonical (dormant) state
    n_frames: usize,
    acc: f64,
    acc_n: usize,
    /// audio delay line (stereo, sized for the longest look-ahead)
    dl: Vec<f32>,
    dr: Vec<f32>,
    w: usize,
    /// samples output since the canonical state (index of the input sample being heard)
    out_pos: usize,
    fw_state: f32,
    est: AutoEst,
    quiet: usize,
    dormant: bool,
    m: [f32; 4],
    params: Option<ParamBlock>,
    /// Offline analysis only: per-frame (gain dB, confidence, active) as frames are
    /// finalised. Filled only within the capacity given to [`Rider::with_log`], so the
    /// audio thread never allocates.
    log: Vec<[f32; 3]>,
}

impl Default for Rider {
    fn default() -> Self {
        Self {
            sr: 0.0,
            hop: 480,
            kl: KWeight::default(),
            kr: KWeight::default(),
            amount: 0.5,
            phrase: 0.2,
            up: 5.0,
            down: 6.0,
            speed: 0,
            auto: true,
            target: -24.0,
            floor: -70.0,
            gate: -60.0,
            mode: look::SHORT,
            out: 1.0,
            wf: 20,
            wp: 200,
            fa: 9,
            ap: 0,
            back: 20,
            a_cut: 0.0,
            a_boost: 0.0,
            latency: 0,
            frames: vec![],
            n_frames: 0,
            acc: 0.0,
            acc_n: 0,
            dl: vec![],
            dr: vec![],
            w: 0,
            out_pos: 0,
            fw_state: 0.0,
            est: AutoEst::default(),
            quiet: 0,
            dormant: true,
            m: [0.0; 4],
            params: None,
            log: Vec::new(),
        }
    }
}

impl Rider {
    /// A rider that records its per-frame gain curve (offline analysis and tests).
    pub fn with_log(frames: usize) -> Self {
        Self { log: Vec::with_capacity(frames), ..Self::default() }
    }

    /// The recorded (gain dB, confidence, active) per 10 ms frame, frame 0 first.
    pub fn log(&self) -> &[[f32; 3]] {
        &self.log
    }

    fn apply(&mut self) {
        let Some(p) = self.params else { return };
        let v = p.values;
        self.amount = (v[idx::AMOUNT] / 100.0).clamp(0.0, 1.0);
        self.phrase = (v[idx::PHRASE] / 100.0).clamp(0.0, 1.0);
        self.up = v[idx::UP].clamp(0.0, 24.0);
        self.down = v[idx::DOWN].clamp(0.0, 24.0);
        self.auto = v[idx::AUTO] >= 0.5;
        self.target = v[idx::TARGET];
        self.floor = v[idx::FLOOR];
        self.gate = v[idx::GATE];
        self.out = crate::dsp::db_to_gain(v[idx::OUT].clamp(-24.0, 24.0));
        // speed and look-ahead (window sizes, latency) are switched in `set_params` via
        // `configure`, within the buffers `prepare` sized for the longest look-ahead
    }

    fn configure(&mut self) {
        let v = self.params.map(|p| p.values).unwrap_or_default();
        self.speed = if self.params.is_some() { v[idx::SPEED].round().clamp(0.0, 1.0) as usize } else { 0 };
        self.mode = if self.params.is_some() { look_mode(v[idx::LOOK]) } else { look::SHORT };
        (self.wf, self.wp) = windows(self.speed);
        (self.fa, self.ap, self.back) = ahead(self.speed, self.mode);
        self.hop = hop(self.sr);
        self.latency = latency_samples(self.speed as f32, self.mode as f32, self.sr);
        let live = self.mode == look::LIVE;
        if live {
            // a trailing window lags by its whole length: half of the centred one, and
            // faster slews, so a word is still caught while it lasts
            self.wf = (self.wf / 2).max(2);
        }
        self.a_cut = (-10.0 / if live { LIVE_CUT_MS } else { CUT_MS }).exp();
        self.a_boost = (-10.0 / if live { LIVE_BOOST_MS } else { BOOST_MS }).exp();
    }

    /// The state after a long stretch of digital silence (also the start state).
    fn go_dormant(&mut self) {
        self.frames.fill(Frame { pk: P_MIN, pf: P_MIN, lf: -140.0, lp: -140.0, ..Frame::default() });
        self.n_frames = 0;
        self.acc = 0.0;
        self.acc_n = 0;
        self.dl.fill(0.0);
        self.dr.fill(0.0);
        self.w = 0;
        self.out_pos = 0;
        self.fw_state = 0.0;
        self.kl.reset();
        self.kr.reset();
        self.quiet = 0;
        self.dormant = true;
        self.m = [0.0, -140.0, -140.0, 0.0];
    }

    #[inline]
    fn fr(&self, i: isize) -> &Frame {
        &self.frames[(i.rem_euclid(RING as isize)) as usize]
    }

    /// One more 10 ms frame of input analysed: advance every stage of the pipeline.
    fn push_frame(&mut self, pk: f64) {
        let n = self.n_frames as isize;
        self.frames[n as usize & MASK] = Frame { pk: pk + P_MIN, pf: P_MIN, lf: -140.0, lp: -140.0, ..Frame::default() };
        self.n_frames += 1;

        // 1. word level at m (window offsets −(wf − 1 − fa) .. +fa: centred, or trailing live)
        let m = n - self.fa as isize;
        let behind = (self.wf - 1 - self.fa) as isize;
        let mut s = 0.0;
        for k in (m - behind)..=(m + self.fa as isize) {
            s += self.fr(k).pk;
        }
        let pf = s / self.wf as f64;
        let lf = (10.0 * (pf + P_MIN).log10()) as f32;
        let (floor, target, act_thr) = if self.auto {
            if lf > SILENCE_DB {
                let e = &mut self.est;
                if lf > e.top {
                    e.top += 0.3 * (lf - e.top);
                } else {
                    e.top -= 0.02;
                }
                if lf < e.floor {
                    e.floor += 0.3 * (lf - e.floor);
                } else {
                    e.floor += 0.01;
                }
            }
            let floor = self.est.floor.min(self.est.top - 35.0);
            (floor, self.est.target, self.est.top - 30.0)
        } else {
            (self.floor, self.target, self.gate)
        };
        let u = ((lf - floor - 6.0) / 8.0).clamp(0.0, 1.0);
        let active = lf > act_thr;
        let conf = if active { u * u * (3.0 - 2.0 * u) } else { 0.0 };
        if self.auto && lf > SILENCE_DB && conf > 0.5 {
            let e = &mut self.est;
            if e.seeded {
                e.target += 0.002 * (lf - e.target);
            } else {
                e.target = lf;
                e.seeded = true;
            }
        }
        {
            let f = &mut self.frames[m.rem_euclid(RING as isize) as usize];
            f.pf = pf;
            f.lf = lf;
            f.conf = conf;
            f.active = active;
        }

        // 2. phrase level and desired gain at q (window ends `ap` frames after q)
        let q = m - self.ap as isize;
        let lo = q - (self.wp as isize - 1 - self.ap as isize);
        let (mut num, mut den) = (0.0f64, 0.0f64);
        for k in lo..=m {
            let f = self.fr(k);
            num += f.pk * f.conf as f64;
            den += f.conf as f64;
        }
        let fq = *self.fr(q);
        let p_phrase = if den / self.wp as f64 > 1e-3 { num / den.max(1e-9) } else { fq.pf };
        let lp = (10.0 * (p_phrase + P_MIN).log10()) as f32;
        let e = lp - fq.lf;
        let db = e.signum() * (e.abs() - DEADBAND_DB).max(0.0);
        let mut g = self.amount * db + self.phrase * (target - lp);
        if g > 0.0 {
            g *= fq.conf; // low confidence blocks a boost, never a cut
        }
        if !fq.active {
            g = 0.0; // idle: back to 0 dB
        }
        g = g.clamp(-self.down, self.up);
        // forward slew
        let a = if g < self.fw_state { self.a_cut } else { self.a_boost };
        self.fw_state = a * self.fw_state + (1.0 - a) * g;
        {
            let f = &mut self.frames[q.rem_euclid(RING as isize) as usize];
            f.lp = lp;
            f.g = g;
            f.fw = self.fw_state;
        }

        // 3. backward slew over the look-ahead, averaged with the forward one, at qq
        let qq = q - self.back as isize;
        let mut st = self.fr(q).g;
        let mut k = q - 1;
        while k >= qq {
            let v = self.fr(k).g;
            let a = if v < st { self.a_cut } else { self.a_boost };
            st = a * st + (1.0 - a) * v;
            k -= 1;
        }
        // live: no future frames to slew back from, the forward slew alone
        let gs = if self.mode == look::LIVE { self.fr(qq).fw } else { 0.5 * (self.fr(qq).fw + st) };
        self.frames[qq.rem_euclid(RING as isize) as usize].gs = gs;
        if qq >= 0 && self.log.len() < self.log.capacity() && qq as usize == self.log.len() {
            let f = self.fr(qq);
            self.log.push([gs, f.conf, if f.active { 1.0 } else { 0.0 }]);
        }
    }

    /// Before parking: analyse the silence that processing would have fed until dormancy.
    /// Only the auto-target estimates outlive dormancy, and the last frames of audio still
    /// in the word window update them; without this, a rider parked mid-phrase would carry
    /// other estimates than one that ran into the silence. No audio is produced.
    fn flush_silence(&mut self) {
        if !self.auto || self.dl.is_empty() {
            return;
        }
        for _ in 0..(self.latency + self.hop * 4).saturating_sub(self.quiet) {
            let (ka, kb) = (self.kl.tick(0.0), self.kr.tick(0.0));
            self.acc += 0.5 * (ka as f64 * ka as f64 + kb as f64 * kb as f64);
            self.acc_n += 1;
            if self.acc_n == self.hop {
                let pk = self.acc / self.hop as f64;
                self.acc = 0.0;
                self.acc_n = 0;
                self.push_frame(pk);
            }
        }
    }

    /// Final gain (dB) at input sample `s`: linear between frame centres, like `np.interp`.
    #[inline]
    fn gain_at(&self, s: usize) -> f32 {
        let h = self.hop as isize;
        let u = s as isize - h / 2;
        let q0 = u.div_euclid(h);
        let w = u.rem_euclid(h) as f32 / h as f32;
        let g0 = if q0 < 0 { self.fr(0).gs } else { self.fr(q0).gs };
        let g1 = self.fr(q0 + 1).gs;
        g0 + (g1 - g0) * w
    }

    /// Gain (dB) applied to input sample `s` (live mode: the curve a few frames behind).
    #[inline]
    fn heard_gain(&self, s: usize) -> f32 {
        if self.mode == look::LIVE {
            match s.checked_sub(LIVE_LAG_HOPS * self.hop) {
                Some(t) => self.gain_at(t),
                // before the first analysed frames: unity, like the look-ahead modes
                None => 0.0,
            }
        } else {
            self.gain_at(s)
        }
    }
}

impl Processor for Rider {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        self.configure();
        self.frames = vec![Frame::default(); RING];
        let cap = latency_samples(0.0, 1.0, sr).max(latency_samples(1.0, 1.0, sr)) + 1;
        self.dl = vec![0.0; cap];
        self.dr = vec![0.0; cap];
        self.kl = KWeight::new(sr);
        self.kr = KWeight::new(sr);
        self.apply();
        self.go_dormant();
    }

    fn set_params(&mut self, p: &ParamBlock) {
        self.params = Some(*p);
        let (speed, mode) = (p.values[idx::SPEED].round().clamp(0.0, 1.0) as usize, look_mode(p.values[idx::LOOK]));
        if self.sr > 0.0 && (speed != self.speed || mode != self.mode) {
            // the buffers are sized for the longest look-ahead in `prepare`: no allocation
            self.configure();
            self.go_dormant();
        }
        self.apply();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        if self.dl.is_empty() {
            return;
        }
        // Dormancy (as in `vocal::Tune`): once the look-ahead holds nothing but silence the
        // state is reset to one canonical state, which is also what `park` leaves. "Silence"
        // is below −120 dBFS, not exact zeros: effects before the rider leave decaying
        // residues long after their tails, and the frame phase must not depend on them.
        const QUIET: f32 = 1e-6;
        let silent = l.iter().all(|x| x.abs() < QUIET) && r.iter().all(|x| x.abs() < QUIET);
        if silent {
            if self.dormant {
                l.fill(0.0);
                r.fill(0.0);
                return;
            }
            if self.quiet >= self.latency + self.hop * 4 {
                self.go_dormant();
                l.fill(0.0);
                r.fill(0.0);
                return;
            }
            self.quiet += l.len();
        } else {
            self.quiet = 0;
            self.dormant = false;
        }
        let n = self.dl.len();
        for i in 0..l.len() {
            let (a, b) = (l[i], r[i]);
            let (ka, kb) = (self.kl.tick(a), self.kr.tick(b));
            self.acc += 0.5 * (ka as f64 * ka as f64 + kb as f64 * kb as f64);
            self.acc_n += 1;
            if self.acc_n == self.hop {
                let pk = self.acc / self.hop as f64;
                self.acc = 0.0;
                self.acc_n = 0;
                self.push_frame(pk);
            }
            self.dl[self.w] = a;
            self.dr[self.w] = b;
            // the sample `latency` behind the input
            let rd = (self.w + n - self.latency) % n;
            self.w = (self.w + 1) % n;
            let g = if self.out_pos >= self.latency {
                let s = self.out_pos - self.latency;
                crate::dsp::db_to_gain(self.heard_gain(s)) * self.out
            } else {
                self.out
            };
            l[i] = self.dl[rd] * g;
            r[i] = self.dr[rd] * g;
            self.out_pos += 1;
        }
        if self.out_pos > self.latency {
            let s = self.out_pos - 1 - self.latency;
            let q = (s as isize - self.hop as isize / 2).max(0) / self.hop as isize;
            let f = self.fr(q);
            self.m = [self.heard_gain(s), f.lf, f.lp, f.conf];
        }
    }

    fn reset(&mut self) {
        self.go_dormant();
    }

    fn park(&mut self) {
        if !self.dormant {
            self.flush_silence();
            self.go_dormant();
        }
    }

    fn tail_samples(&self) -> Option<usize> {
        // silent input reaches the output after the look-ahead; the state then goes
        // dormant (canonical) a few frames later
        Some(self.latency + self.hop * 8 + 4096)
    }

    fn gain_reduction_db(&self) -> f32 {
        self.m[0].min(0.0)
    }

    fn meters(&self, out: &mut [f32; METERS]) {
        out[..4].copy_from_slice(&self.m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autovocal::{metrics, render, rider_curve, rider_levels, rider_params, Style};
    use crate::catalog;
    use crate::model::FxKind;
    use std::collections::BTreeMap;

    const SR: f32 = 44100.0;

    fn params(kv: &[(&str, f64)]) -> BTreeMap<String, f64> {
        let mut m = catalog::default_params(FxKind::Rider);
        for (k, v) in kv {
            m.insert((*k).to_owned(), *v);
        }
        m
    }

    fn noise(seed: u32) -> impl FnMut() -> f32 {
        let mut s = seed.max(1);
        move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32 - 0.5) * 2.0
        }
    }

    /// "Words": harmonic bursts of `len` s at the given dBFS levels, `gap` s apart, over a
    /// −70 dBFS noise bed.
    fn words(levels: &[f32], len: f32, gap: f32) -> Vec<f32> {
        let mut x = vec![];
        let mut nz = noise(1);
        for &db in levels {
            let a = 10f32.powf(db / 20.0);
            for i in 0..(len * SR) as usize {
                let t = i as f32 / SR;
                let env = (t / 0.02).min(1.0) * ((len - t) / 0.02).min(1.0);
                let w = 2.0 * std::f32::consts::PI * 220.0 * t;
                let v = w.sin() + 0.5 * (3.0 * w).sin() + 0.3 * (6.0 * w).sin();
                x.push(a * env * v / 1.8 + 3e-4 * nz());
            }
            for _ in 0..(gap * SR) as usize {
                x.push(3e-4 * nz());
            }
        }
        x
    }

    fn measured(x: &[f32]) -> BTreeMap<String, f64> {
        let (t, floor, top, _) = rider_levels(x, SR, Style::Natural);
        rider_params(t, floor, top - 30.0, Style::Natural, true)
    }

    #[test]
    fn latency_is_what_the_catalog_reports() {
        for look in [0.0, 1.0, 2.0] {
            for speed in [0.0, 1.0] {
                // amount and phrase at 0: unity gain, so output = input delayed by the latency
                let p = params(&[("amount", 0.0), ("phrase", 0.0), ("look", look), ("speed", speed), ("auto", 0.0)]);
                let x = words(&[-12.0, -20.0, -15.0], 0.3, 0.1);
                let y = render(FxKind::Rider, &p, &x, SR);
                let d = x.iter().zip(&y).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
                assert!(d < 1e-6, "look {look} speed {speed}: output differs from the input by {d}");
            }
        }
        let mut fx = catalog::new_fx(1, FxKind::Rider, 120.0);
        fx.params.insert("look".into(), 1.0);
        assert_eq!(catalog::latency(&fx, 44100.0), (9 + 99 + 50 + 2) * 441);
        fx.params.insert("look".into(), 0.0);
        assert_eq!(catalog::latency(&fx, 48000.0), (9 + 20 + 2) * 480);
        fx.params.insert("look".into(), 2.0);
        assert_eq!(catalog::latency(&fx, 48000.0), 0, "live: no delay");
        assert_eq!(catalog::new_fx(2, FxKind::Rider, 120.0).params["look"], 2.0, "a new rider is live");
    }

    #[test]
    fn live_mode_evens_words_without_delay() {
        let lv: Vec<f32> = (0..24).map(|i| if (i / 2) % 2 == 0 { -14.0 } else { -24.0 }).collect();
        let x = words(&lv, 0.5, 0.08);
        let mut p = measured(&x);
        p.insert("look".into(), 2.0);
        let y = render(FxKind::Rider, &p, &x, SR);
        let segs = metrics::words(&x, SR);
        let (_, a, _) = metrics::m2(&x, SR, &segs);
        let (_, b, _) = metrics::m2(&y, SR, &segs);
        assert!(a - b > 2.0, "live: word spread {a:.2} → {b:.2} dB: not evened");
        assert!(b > 1.0, "live: word spread {a:.2} → {b:.2} dB: flattened completely");
        // (zero delay itself: `latency_is_what_the_catalog_reports`)
    }

    #[test]
    fn evens_words_toward_their_phrase() {
        // a phrase of alternating loud/quiet words, 10 dB apart
        let lv: Vec<f32> = (0..24).map(|i| if (i / 2) % 2 == 0 { -14.0 } else { -24.0 }).collect();
        let x = words(&lv, 0.5, 0.08);
        let y = render(FxKind::Rider, &measured(&x), &x, SR);
        let segs = metrics::words(&x, SR);
        let (_, a, _) = metrics::m2(&x, SR, &segs);
        let (_, b, _) = metrics::m2(&y, SR, &segs);
        assert!(a - b > 3.0, "word spread {a:.2} → {b:.2} dB: not evened");
        assert!(b > 1.0, "word spread {a:.2} → {b:.2} dB: flattened completely");
    }

    #[test]
    fn breaths_and_noise_are_not_lifted() {
        // loud phrase, 2 s of room noise / breath at about −50 dBFS, loud phrase
        let mut x = words(&[-14.0, -16.0, -14.0, -15.0], 0.3, 0.05);
        let n0 = x.len();
        let mut nz = noise(7);
        for _ in 0..(2.0 * SR) as usize {
            x.push(0.003 * nz());
        }
        let n1 = x.len();
        x.extend(words(&[-14.0, -16.0, -15.0], 0.3, 0.05));
        let (_, log) = rider_curve(&measured(&x), &x, SR);
        let h = hop(SR);
        // the middle of the noise stretch (away from the slews at its edges)
        let max = log[(n0 / h + 60)..(n1 / h - 60)].iter().fold(f32::MIN, |m, f| m.max(f[0]));
        assert!(max < 0.25, "noise lifted by up to {max:.2} dB");
    }

    #[test]
    fn auto_target_converges_and_rides() {
        let lv: Vec<f32> = (0..60).map(|i| if (i / 2) % 2 == 0 { -14.0 } else { -24.0 }).collect();
        let x = words(&lv, 0.5, 0.08);
        let y = render(FxKind::Rider, &params(&[("auto", 1.0)]), &x, SR);
        // second half: the estimates have settled
        let h = x.len() / 2;
        let segs = metrics::words(&x[h..], SR);
        let (_, a, _) = metrics::m2(&x[h..], SR, &segs);
        let (_, b, _) = metrics::m2(&y[h..], SR, &segs);
        assert!(a - b > 2.0, "auto mode: word spread {a:.2} → {b:.2} dB");
    }

    #[test]
    fn parking_equals_processing_silence() {
        for look in [0.0, 1.0, 2.0] {
            parking_equals_processing_silence_at(look);
        }
    }

    fn parking_equals_processing_silence_at(look: f64) {
        let blk = 512;
        // longer than the long look-ahead, so the gain after parking is heard
        let x = words(&[-12.0, -20.0, -14.0, -18.0, -10.0], 0.3, 0.1);
        let mut fx = catalog::new_fx(1, FxKind::Rider, 120.0);
        fx.params.insert("look".into(), look);
        assert_eq!(fx.params["auto"], 1.0, "the auto-target estimates are what outlives parking");
        let pb = catalog::param_block(&fx);
        let ctx = ProcessCtx { sample_rate: SR, bpm: 120.0, beat: 0.0, playing: true };
        let mk = || {
            let mut r = Rider::default();
            r.prepare(SR, blk);
            r.set_params(&pb);
            r
        };
        let run = |r: &mut Rider, sig: &[f32]| -> Vec<f32> {
            let mut out = vec![];
            for c in sig.chunks(blk) {
                let (mut l, mut rr) = (c.to_vec(), c.to_vec());
                r.process(&mut l, &mut rr, &ctx);
                out.extend(l);
            }
            out
        };
        let (mut a, mut b) = (mk(), mk());
        run(&mut a, &x);
        run(&mut b, &x);
        let zeros = vec![0f32; (a.tail_samples().unwrap() / blk + 4) * blk];
        let za = run(&mut a, &zeros);
        assert!(za[za.len() - blk..].iter().all(|v| *v == 0.0));
        b.park();
        let (ya, yb) = (run(&mut a, &x), run(&mut b, &x));
        assert!(ya.iter().any(|v| v.abs() > 1e-3));
        assert_eq!(ya, yb, "look {look}: a parked rider must continue exactly like one that processed the silence");
    }
}
