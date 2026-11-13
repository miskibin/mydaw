//! Vocal processors: FET compressor (1176 style), de-esser, real-time pitch correction,
//! voice character (pitch / formant / robot / drive).
//!
//! Meter layout (`Processor::meters`, documented in docs/architecture.md):
//! * fet:   [GR dB, input dB, output dB]
//! * deess: [GR dB, sidechain dB (Relative: high/low band ratio dB), sibilance 0..1, threshold dB]
//! * tune:  [input Hz (0 = unvoiced), target Hz, output Hz, confidence, input cents off target, applied correction cents]
//! * voice: [input Hz (0 = unvoiced), output Hz, confidence, applied shift st]

use crate::dsp::{db_to_gain, gain_to_db, Biquad, FilterType, Smooth};
use crate::pitch::{hz_to_midi, midi_to_hz, scale_mask, snap, Detector, Psola};
use crate::processor::{ParamBlock, ProcessCtx, Processor, METERS};

// ------------------------------------------------------------------ FET compressor

/// Fixed-threshold FET-style compressor: drive into it with Input, choose a ratio
/// button (4/8/12/20/All), attack 20–800 µs, release 50 ms–1.1 s with a
/// program-dependent second stage, and a class-A style output stage that adds
/// low-order harmonics as you push it.
pub struct Fet {
    sr: f32,
    input: f32,
    output: f32,
    ratio: f32,
    abi: bool,
    att: f32,
    rel_fast: f32,
    rel_slow: f32,
    mix: f32,
    env: f32,
    slow: f32,
    dc_l: (f32, f32),
    dc_r: (f32, f32),
    gr: f32,
    in_db: f32,
    out_db: f32,
}

impl Default for Fet {
    fn default() -> Self {
        Self {
            sr: 48000.0,
            input: 1.0,
            output: 1.0,
            ratio: 4.0,
            abi: false,
            att: 0.0,
            rel_fast: 0.0,
            rel_slow: 0.0,
            mix: 1.0,
            env: 0.0,
            slow: 0.0,
            dc_l: (0.0, 0.0),
            dc_r: (0.0, 0.0),
            gr: 0.0,
            in_db: -120.0,
            out_db: -120.0,
        }
    }
}

/// Fixed threshold (like the hardware): a vocal peaking around −10 dBFS starts
/// compressing with the Input knob at about +6 dB.
const FET_THRESHOLD_DB: f32 = -10.0;

impl Fet {
    #[inline]
    fn static_gr(&self, x_db: f32) -> f32 {
        let over = x_db - FET_THRESHOLD_DB;
        let knee = if self.abi {
            2.0
        } else if self.ratio <= 4.0 {
            8.0
        } else {
            4.0
        };
        let slope = 1.0 / self.ratio - 1.0;
        let mut gr = if over <= -knee / 2.0 {
            0.0
        } else if over >= knee / 2.0 {
            slope * over
        } else {
            let t = over + knee / 2.0;
            slope * t * t / (2.0 * knee)
        };
        if self.abi && over > 0.0 {
            // the all-buttons "plateau": over-compresses just above threshold, then relaxes slightly
            gr -= 4.0 * (over / 6.0).min(1.0) * (1.0 - (over / 30.0).min(1.0));
        }
        gr
    }
    #[inline]
    fn color(x: f32, drive: f32, dc: &mut (f32, f32)) -> f32 {
        // mostly-even harmonics, then a DC blocker
        let k = 0.06 * drive;
        let y = x + k * x * x - 0.5 * k * x * x * x;
        let o = y - dc.0 + 0.9995 * dc.1;
        dc.0 = y;
        dc.1 = o;
        o.clamp(-1.5, 1.5)
    }
}

impl Processor for Fet {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
    }
    fn set_params(&mut self, p: &ParamBlock) {
        let v = p.values;
        self.input = db_to_gain(v[0]);
        self.output = db_to_gain(v[1]);
        let r = v[2].round() as i32;
        self.abi = r >= 4;
        self.ratio = [4.0, 8.0, 12.0, 20.0, 20.0][r.clamp(0, 4) as usize];
        let mut att_us = v[3].clamp(20.0, 800.0);
        let mut rel_ms = v[4].clamp(50.0, 1100.0);
        if self.abi {
            // ABI: faster attack, slower recovery (the bias shift people hear as "pumping")
            att_us *= 0.6;
            rel_ms *= 1.4;
        }
        self.att = (-1.0 / (self.sr * att_us / 1e6)).exp();
        self.rel_fast = (-1.0 / (self.sr * rel_ms / 1000.0)).exp();
        self.rel_slow = (-1.0 / (self.sr * rel_ms * 3.0 / 1000.0)).exp();
        self.mix = v[5] / 100.0;
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        let mut gr_min = 0f32;
        let (mut pin, mut pout) = (0f32, 0f32);
        let drive = (self.input.log10() * 20.0 / 24.0).clamp(0.0, 1.5) + if self.abi { 0.8 } else { 0.0 };
        for i in 0..l.len() {
            let (a, b) = (l[i] * self.input, r[i] * self.input);
            let lvl = a.abs().max(b.abs());
            pin = pin.max(lvl);
            let target = self.static_gr(gain_to_db(lvl).max(-120.0));
            if target < self.env {
                self.env = target + (self.env - target) * self.att;
            } else {
                // program-dependent release: long stretches of reduction recover slower
                let k = self.rel_fast + (self.rel_slow - self.rel_fast) * self.slow;
                self.env = target + (self.env - target) * k;
            }
            self.slow += ((if self.env < -3.0 { 1.0 } else { 0.0 }) - self.slow) * 0.00002;
            let g = db_to_gain(self.env);
            let (wa, wb) =
                (Self::color(a * g, drive, &mut self.dc_l) * self.output, Self::color(b * g, drive, &mut self.dc_r) * self.output);
            let (da, db) = (l[i], r[i]);
            l[i] = da * (1.0 - self.mix) + wa * self.mix;
            r[i] = db * (1.0 - self.mix) + wb * self.mix;
            pout = pout.max(l[i].abs().max(r[i].abs()));
            gr_min = gr_min.min(self.env);
        }
        self.gr = gr_min;
        self.in_db = gain_to_db(pin);
        self.out_db = gain_to_db(pout);
    }
    fn reset(&mut self) {
        self.env = 0.0;
        self.slow = 0.0;
    }
    fn tail_samples(&self) -> Option<usize> {
        // gain-reduction envelope (slow release) and the DC blocker (pole 0.9995)
        Some(crate::dsp::decay_samples(self.rel_slow, 17.0).max(crate::dsp::decay_samples(self.rel_fast, 17.0)) + 40_000)
    }
    fn park(&mut self) {
        self.reset();
        self.dc_l = (0.0, 0.0);
        self.dc_r = (0.0, 0.0);
    }
    fn gain_reduction_db(&self) -> f32 {
        self.gr
    }
    fn meters(&self, out: &mut [f32; METERS]) {
        out[0] = self.gr;
        out[1] = self.in_db;
        out[2] = self.out_db;
    }
}

// ------------------------------------------------------------------ de-esser

/// De-esser. The detector looks at a high band (`freq` and up) *and* at how much
/// of the signal's energy sits there: an "s" is mostly high-band energy, a bright
/// sung vowel is not. Reduction needs both — so cymbal-bright vocals are left
/// alone, sibilants are caught. Split band ducks only the highs; wide band ducks
/// everything. Look-ahead delays the audio so the gain is down before the "s".
pub struct DeEss {
    sr: f32,
    thr: f32,
    range: f32,
    freq: f32,
    split: bool,
    look: usize,
    listen: bool,
    hp_det: Biquad,
    lp_l: Biquad,
    lp_r: Biquad,
    lp_l2: Biquad,
    lp_r2: Biquad,
    env_hi: f32,
    env_all: f32,
    gr: f32,
    gr_env: f32,
    dl: Vec<f32>,
    dr: Vec<f32>,
    w: usize,
    att: f32,
    rel: f32,
    ratio_now: f32,
    side_db: f32,
    params: Option<ParamBlock>,
    /// Relative detection: the threshold is on the high/low band energy ratio (dB), so it
    /// does not depend on the take's level (the auto-vocal research de-esser).
    relative: bool,
    /// Relative mode splits like the research de-esser: high = 4th-order Butterworth
    /// high-pass, low = input − high (stages per channel; [4], [5] = detector).
    hp4: [[Biquad; 2]; 3],
    env_rh: f32,
    env_rl: f32,
    k_rel_env: f32,
    rel_att: f32,
    rel_rel: f32,
}

impl Default for DeEss {
    fn default() -> Self {
        Self {
            sr: 48000.0,
            thr: -30.0,
            range: 8.0,
            freq: 6000.0,
            split: true,
            look: 0,
            listen: false,
            hp_det: Biquad::default(),
            lp_l: Biquad::default(),
            lp_r: Biquad::default(),
            lp_l2: Biquad::default(),
            lp_r2: Biquad::default(),
            env_hi: 0.0,
            env_all: 0.0,
            gr: 0.0,
            gr_env: 0.0,
            dl: vec![],
            dr: vec![],
            w: 0,
            att: 0.0,
            rel: 0.0,
            ratio_now: 0.0,
            side_db: -120.0,
            params: None,
            relative: false,
            hp4: [[Biquad::default(); 2]; 3],
            env_rh: 0.0,
            env_rl: 0.0,
            k_rel_env: 0.0,
            rel_att: 0.0,
            rel_rel: 0.0,
        }
    }
}

impl DeEss {
    fn apply(&mut self) {
        let Some(p) = self.params else { return };
        let v = p.values;
        self.thr = v[0];
        self.range = v[1].max(0.0);
        self.freq = v[2].clamp(2000.0, 16000.0);
        self.split = v[3] < 0.5;
        self.look = ((v[4].clamp(0.0, 15.0) / 1000.0) * self.sr) as usize;
        self.listen = v[5] > 0.5;
        self.relative = v[6] > 0.5;
        self.hp_det.set(FilterType::Highpass, self.sr, self.freq, 0.707, 0.0);
        // complementary split: low = LR4 lowpass, high = input − low
        for f in [&mut self.lp_l, &mut self.lp_r, &mut self.lp_l2, &mut self.lp_r2] {
            f.set(FilterType::Lowpass, self.sr, self.freq, 0.707, 0.0);
        }
        for ch in self.hp4.iter_mut() {
            ch[0].set(FilterType::Highpass, self.sr, self.freq, 0.541_196_1, 0.0);
            ch[1].set(FilterType::Highpass, self.sr, self.freq, 1.306_563, 0.0);
        }
    }
}

impl Processor for DeEss {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        let n = (sr * 0.016) as usize + 2;
        self.dl = vec![0.0; n];
        self.dr = vec![0.0; n];
        self.att = (-1.0 / (sr * 0.0008)).exp();
        self.rel = (-1.0 / (sr * 0.06)).exp();
        // relative detector: ~5 ms band energies, 1 ms attack / 40 ms release on the gain
        self.k_rel_env = 1.0 - (-1.0 / (sr * 0.0025)).exp();
        self.rel_att = (-1.0 / (sr * 0.001)).exp();
        self.rel_rel = (-1.0 / (sr * 0.04)).exp();
        self.apply();
    }
    fn set_params(&mut self, p: &ParamBlock) {
        self.params = Some(*p);
        self.apply();
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        let n = self.dl.len();
        if n == 0 {
            return;
        }
        let mut gr_min = 0f32;
        let mut side_pk = 0f32;
        let mut ratio_pk = 0f32;
        for i in 0..l.len() {
            let mono = (l[i] + r[i]) * 0.5;
            if self.relative {
                // band-energy ratio of the complementary split, threshold on the ratio (1:∞)
                let [d1, d2] = &mut self.hp4[2];
                let hi = d2.tick(d1.tick(mono));
                let lo = mono - hi;
                self.env_rh += (hi * hi - self.env_rh) * self.k_rel_env;
                self.env_rl += (lo * lo - self.env_rl) * self.k_rel_env;
                let r_db = 10.0 * (self.env_rh.max(1e-14) / self.env_rl.max(1e-14)).log10();
                let audible = self.env_rl + self.env_rh > 1e-8; // above -80 dBFS
                let target = if audible { -(r_db - self.thr).clamp(0.0, self.range) } else { 0.0 };
                let k = if target < self.gr_env { self.rel_att } else { self.rel_rel };
                self.gr_env = target + (self.gr_env - target) * k;
                side_pk = side_pk.max(r_db);
                ratio_pk = ratio_pk.max(((r_db - self.thr + 6.0) / 12.0).clamp(0.0, 1.0));
            } else {
                let hi = self.hp_det.tick(mono);
                // detector envelopes (energy)
                let k_hi = if hi * hi > self.env_hi { 0.2 } else { 0.002 };
                self.env_hi += (hi * hi - self.env_hi) * k_hi;
                let k_all = if mono * mono > self.env_all { 0.2 } else { 0.002 };
                self.env_all += (mono * mono - self.env_all) * k_all;
                let hi_db = 10.0 * self.env_hi.max(1e-12).log10();
                let ratio = (self.env_hi / self.env_all.max(1e-12)).clamp(0.0, 1.0);
                // how "s"-like: high band holds more than ~35 % of the energy
                let sib = ((ratio - 0.25) / 0.35).clamp(0.0, 1.0);
                let over = (hi_db - self.thr).max(0.0);
                let target = -(over * 0.8 * sib).min(self.range);
                let k = if target < self.gr_env { self.att } else { self.rel };
                self.gr_env = target + (self.gr_env - target) * k;
                side_pk = side_pk.max(hi_db);
                ratio_pk = ratio_pk.max(sib);
            }
            gr_min = gr_min.min(self.gr_env);
            // delayed audio
            self.dl[self.w] = l[i];
            self.dr[self.w] = r[i];
            let rd = (self.w + n - self.look.min(n - 1)) % n;
            self.w = (self.w + 1) % n;
            let (a, b) = (self.dl[rd], self.dr[rd]);
            let g = db_to_gain(self.gr_env);
            let (la, lb, ha, hb) = if self.relative {
                let [[l1, l2], [r1, r2], _] = &mut self.hp4;
                let (ha, hb) = (l2.tick(l1.tick(a)), r2.tick(r1.tick(b)));
                (a - ha, b - hb, ha, hb)
            } else {
                let (la, lb) = (self.lp_l2.tick(self.lp_l.tick(a)), self.lp_r2.tick(self.lp_r.tick(b)));
                (la, lb, a - la, b - lb)
            };
            if self.listen {
                // "Tylko S": only what the de-esser takes away (delta), not the whole high band
                let cut = 1.0 - g;
                if self.split {
                    l[i] = ha * cut;
                    r[i] = hb * cut;
                } else {
                    l[i] = a * cut;
                    r[i] = b * cut;
                }
            } else if self.split {
                l[i] = la + ha * g;
                r[i] = lb + hb * g;
            } else {
                l[i] = a * g;
                r[i] = b * g;
            }
        }
        self.gr = gr_min;
        self.side_db = side_pk;
        self.ratio_now = ratio_pk;
    }
    fn reset(&mut self) {
        self.dl.fill(0.0);
        self.dr.fill(0.0);
        self.gr_env = 0.0;
    }
    fn tail_samples(&self) -> Option<usize> {
        // look-ahead line, split-band filters, detector envelopes (pole 0.998), gain release
        Some(self.dl.len() + crate::dsp::decay_samples(self.rel, 17.0) + 17 * 500 + 4096)
    }
    fn park(&mut self) {
        self.reset();
        self.hp_det.reset();
        for f in [&mut self.lp_l, &mut self.lp_r, &mut self.lp_l2, &mut self.lp_r2] {
            f.reset();
        }
        for f in self.hp4.iter_mut().flatten() {
            f.reset();
        }
        self.env_hi = 0.0;
        self.env_all = 0.0;
        self.env_rh = 0.0;
        self.env_rl = 0.0;
    }
    fn gain_reduction_db(&self) -> f32 {
        self.gr
    }
    fn meters(&self, out: &mut [f32; METERS]) {
        out[0] = self.gr;
        out[1] = self.side_db;
        out[2] = self.ratio_now;
        out[3] = self.thr;
    }
}

// ------------------------------------------------------------------ real-time pitch correction

/// Real-time pitch correction: detect (YIN), pick the nearest note of the key/scale,
/// glide there at the Retune speed, shift with PSOLA (formants stay put).
/// * Humanize: long held notes get a slower retune, so they don't freeze.
/// * Flex: only notes already near the target are corrected; big intentional
///   bends and scoops pass untouched.
/// * Vibrato: how much of the singer's vibrato survives (corrects the note centre,
///   not every wobble).
pub struct Tune {
    sr: f32,
    det: Option<Detector>,
    psola_l: Option<Psola>,
    /// delays the audio into PSOLA so the grain being shifted is the moment the
    /// detector's estimate describes (the centre of its analysis window)
    align: Vec<f32>,
    align_w: usize,
    key: u32,
    mask: u16,
    retune_ms: f32,
    humanize: f32,
    flex: f32,
    vibrato: f32,
    transpose: f32,
    formant: f32,
    period: f32,
    corr: f32,
    center: f32,
    held: f32,
    last_note: f32,
    /// the current note was close enough to engage correction (flex hysteresis)
    engaged: bool,
    /// last accepted pitch (MIDI) and how long a big jump away from it has lasted (s)
    last_m: f32,
    jump_t: f32,
    /// pitch smoothed over ~50 ms and its speed (semitones/s): glides and scoops
    smooth_m: f32,
    speed: f32,
    /// time the voice has sat on the current note centre (s)
    seg_t: f32,
    m: [f32; 8],
    params: Option<ParamBlock>,
    /// Samples of digital silence at the input, and whether the state is the canonical idle one.
    quiet: usize,
    dormant: bool,
}

impl Default for Tune {
    fn default() -> Self {
        Self {
            sr: 48000.0,
            det: None,
            psola_l: None,
            align: vec![],
            align_w: 0,
            key: 0,
            mask: 0xfff,
            retune_ms: 20.0,
            humanize: 0.0,
            flex: 0.0,
            vibrato: 0.0,
            transpose: 0.0,
            formant: 0.0,
            period: 0.0,
            corr: 0.0,
            center: 0.0,
            held: 0.0,
            last_note: 0.0,
            engaged: false,
            last_m: 0.0,
            jump_t: 0.0,
            smooth_m: 0.0,
            speed: 0.0,
            seg_t: 0.0,
            m: [0.0; 8],
            params: None,
            quiet: 0,
            dormant: true,
        }
    }
}

/// Samples of delay the corrector adds at `sr` (reported for latency compensation).
pub fn tune_latency(sr: f32) -> usize {
    Psola::new(sr).latency() + align_delay(sr)
}

/// Detector window centre minus PSOLA's own look-back: the pitch estimate refers to
/// audio that much older than the grain PSOLA is shifting at the same moment.
fn align_delay(sr: f32) -> usize {
    Detector::new(sr).latency().saturating_sub(Psola::new(sr).half_grain())
}

impl Tune {
    fn go_dormant(&mut self) {
        if let Some(d) = self.det.as_mut() {
            d.clear();
        }
        if let Some(p) = self.psola_l.as_mut() {
            p.clear();
        }
        self.align.fill(0.0);
        self.align_w = 0;
        self.period = 0.0;
        self.corr = 0.0;
        self.center = 0.0;
        self.held = 0.0;
        self.last_note = 0.0;
        self.engaged = false;
        self.last_m = 0.0;
        self.jump_t = 0.0;
        self.smooth_m = 0.0;
        self.speed = 0.0;
        self.seg_t = 0.0;
        self.m = [0.0; 8];
        self.quiet = 0;
        self.dormant = true;
    }
}

impl Processor for Tune {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        self.quiet = 0;
        self.dormant = true;
        self.det = Some(Detector::new(sr));
        self.psola_l = Some(Psola::new(sr));
        self.align = vec![0.0; align_delay(sr)];
        self.align_w = 0;
        if let Some(p) = self.params {
            self.set_params(&p);
        }
    }
    fn set_params(&mut self, p: &ParamBlock) {
        self.params = Some(*p);
        let v = p.values;
        self.key = (v[0].round() as i32).rem_euclid(12) as u32;
        self.mask = scale_mask(v[1].round().max(0.0) as u32);
        self.retune_ms = v[2].max(0.0);
        self.humanize = v[3] / 100.0;
        self.flex = v[4] / 100.0;
        self.vibrato = v[5] / 100.0;
        self.transpose = v[6];
        self.formant = v[7];
        // input type → pitch search range
        let (lo, hi) = match v[8].round() as i32 {
            1 => (50.0, 260.0),
            2 => (70.0, 400.0),
            3 => (130.0, 800.0),
            4 => (180.0, 1100.0),
            _ => (crate::pitch::MIN_HZ, crate::pitch::MAX_HZ),
        };
        if let Some(d) = self.det.as_mut() {
            d.set_range(lo, hi);
        }
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        // Dormancy: digital silence for a second flushes everything the effect holds, so
        // it settles to one canonical state and does nothing until sound returns. A chain
        // that is skipped (`Processor::park`) is in exactly this state, so skipping it
        // changes nothing.
        let silent = l.iter().all(|&x| x == 0.0) && r.iter().all(|&x| x == 0.0);
        if silent {
            if self.dormant {
                return;
            }
            if self.quiet >= self.sr as usize {
                self.go_dormant();
                return;
            }
            self.quiet += l.len();
        } else {
            self.quiet = 0;
            self.dormant = false;
        }
        let (Some(det), Some(ps)) = (self.det.as_mut(), self.psola_l.as_mut()) else { return };
        let sr = self.sr;
        for i in 0..l.len() {
            let x = (l[i] + r[i]) * 0.5;
            if det.push(x) {
                // a new estimate every ~4 ms: update the correction target
                let hop_s = 0.004f32;
                // a single estimate far from the line being sung is usually the detector
                // locking onto a loud harmonic for a frame or two: hold course until the
                // jump has lasted 40 ms (then it is a real leap)
                let mut glitch = false;
                if det.hz > 0.0 && det.confidence > 0.55 && self.last_m > 0.0 && (hz_to_midi(det.hz) - self.last_m).abs() > 5.0 {
                    self.jump_t += 0.004;
                    glitch = self.jump_t < 0.04;
                } else {
                    self.jump_t = 0.0;
                }
                if glitch {
                    // keep period and correction as they are
                } else if det.hz > 0.0 && det.confidence > 0.55 {
                    let m = hz_to_midi(det.hz);
                    self.last_m = m;
                    det.hint = det.hz;
                    self.period = sr / det.hz;
                    // pitch speed on a 50 ms-smoothed curve (vibrato ≈ 6 Hz mostly averages out):
                    // tells a voice travelling between notes from one sitting on a note
                    let prev = self.smooth_m;
                    self.smooth_m = if prev == 0.0 { m } else { prev + (m - prev) * (1.0 - (-hop_s / 0.05f32).exp()) };
                    let v = if prev == 0.0 { 0.0 } else { (self.smooth_m - prev).abs() / hop_s };
                    self.speed += (v - self.speed) * 0.3;
                    // note centre (Melodyne's "pitch centre"): the mean pitch since the voice
                    // settled on this note — restarted at a transition or a jump of over a
                    // semitone, a true mean for the first 0.3 s, then slowly following drift.
                    // Correcting this (Vibrato = 100 %) moves the note and keeps every wobble.
                    if self.center == 0.0 || self.speed > 20.0 || (m - self.center).abs() > 1.0 {
                        self.center = m;
                        self.seg_t = 0.0;
                    } else {
                        self.seg_t += hop_s;
                        self.center += (m - self.center) * (hop_s / self.seg_t.max(hop_s)).max(hop_s / 0.3);
                    }
                    let follow = self.center * self.vibrato + m * (1.0 - self.vibrato);
                    let note = snap(follow, self.key, self.mask);
                    if note != self.last_note {
                        self.last_note = note;
                        self.held = 0.0;
                        self.engaged = false;
                    } else {
                        self.held += hop_s;
                    }
                    let err = note - follow; // semitones to correct
                                             // flex: only notes sung within ±(50 − 35·flex) cents of the target are corrected;
                                             // once a note engages it stays corrected (no flicker with vibrato)
                    let half = 0.5 - 0.35 * self.flex; // semitones
                    if self.flex <= 0.0 || err.abs() <= half {
                        self.engaged = true;
                    }
                    // glide protection (natural modes): while the voice is travelling between
                    // notes — a scoop into a note, a slide, a fall — leave it alone and only
                    // pull once it settles. Measured on a 50 ms-smoothed pitch so vibrato
                    // (≈ 6 Hz) doesn't count as travelling. Hard tuning (retune < 5 ms) keeps
                    // the classic stepped effect.
                    let glide = if self.retune_ms >= 5.0 { (1.0 - (self.speed - 10.0) / 15.0).clamp(0.0, 1.0) } else { 1.0 };
                    let want = if self.engaged { err * glide } else { 0.0 };
                    let mut tau = self.retune_ms / 1000.0;
                    if self.humanize > 0.0 && self.held > 0.25 {
                        tau += self.humanize * 0.4 * ((self.held - 0.25) / 0.5).min(1.0);
                    }
                    let k = if tau <= 0.0005 { 1.0 } else { 1.0 - (-hop_s / tau).exp() };
                    self.corr += (want - self.corr) * k;
                    self.m = [
                        det.hz,
                        midi_to_hz(note + self.transpose),
                        midi_to_hz(m + self.corr + self.transpose),
                        det.confidence,
                        (m - note) * 100.0,
                        self.corr * 100.0,
                        0.0,
                        0.0,
                    ];
                } else {
                    self.period = 0.0;
                    self.corr *= 0.9;
                    self.center = 0.0;
                    self.last_m = 0.0;
                    det.hint = 0.0;
                    self.smooth_m = 0.0;
                    self.speed = 0.0;
                    self.m = [0.0, 0.0, 0.0, det.confidence, 0.0, 0.0, 0.0, 0.0];
                }
            }
            let ratio = 2f32.powf((self.corr + self.transpose) / 12.0);
            let formant = 2f32.powf(self.formant / 12.0); // PSOLA keeps formants; this moves them on purpose
            let n = self.align.len();
            let xd = if n > 0 {
                let d = self.align[self.align_w];
                self.align[self.align_w] = x;
                self.align_w = (self.align_w + 1) % n;
                d
            } else {
                x
            };
            let y = ps.process(xd, self.period, ratio, formant);
            l[i] = y;
            r[i] = y;
        }
    }
    fn reset(&mut self) {
        if let Some(p) = self.psola_l.as_mut() {
            p.reset();
        }
        self.align.fill(0.0);
        self.corr = 0.0;
    }
    fn park(&mut self) {
        if !self.dormant {
            self.go_dormant();
        }
    }
    fn tail_samples(&self) -> Option<usize> {
        // look-ahead + pitch window + grain buffers all hold well under a second
        Some(self.align.len() + (self.sr * 1.5) as usize)
    }
    fn meters(&self, out: &mut [f32; METERS]) {
        out[..8].copy_from_slice(&self.m);
    }
}

// ------------------------------------------------------------------ voice character

/// Robot mode holds the voice on this note (+ Pitch).
const ROBOT_NOTE: f32 = 60.0;
/// Voice's drive: the character saturator's triode voicing, level-matched, with a
/// light 6 dB/oct roll-off at 14 kHz, at 2× oversampling.
fn voice_sat(drive_db: f32) -> crate::dsp::saturator::Settings {
    use crate::dsp::saturator as s;
    s::Settings {
        style: s::TRIODE,
        drive: s::drive_pos(drive_db),
        highcut: 14000.0,
        auto: true,
        // fully clean at 0 dB, fully driven from 3 dB
        mix: (drive_db / 3.0).clamp(0.0, 1.0),
        ..Default::default()
    }
}

/// Voice character for a clean monophonic vocal (Little AlterBoy style), on the
/// same detector and PSOLA as the corrector:
/// * Transpose: shift by Pitch; formants stay unless Formant moves them.
/// * Quantize: snap to the nearest semitone at once (hard tune), then shift.
/// * Robot: hold one note (C4 + Pitch) whatever is sung.
///
/// Link moves the formants with the pitch shift (the varispeed sound). Drive is the
/// character saturator (triode voicing, auto gain) after the shift, clean at 0 dB. Its
/// oversampling delay is taken out of the look-ahead, so the total latency is unchanged.
pub struct Voice {
    sr: f32,
    det: Option<Detector>,
    psola: Option<Psola>,
    align: Vec<f32>,
    align_w: usize,
    mode: u32,
    pitch: f32,
    formant: f32,
    link: bool,
    drive_db: f32,
    sat: crate::dsp::saturator::Core,
    out: f32,
    period: f32,
    /// semitones the current estimate asks for, and the glided value PSOLA gets
    want: f32,
    shift: f32,
    shift_k: f32,
    last_m: f32,
    jump_t: f32,
    so: Smooth,
    m: [f32; 4],
    params: Option<ParamBlock>,
    quiet: usize,
    dormant: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            sr: 48000.0,
            det: None,
            psola: None,
            align: vec![],
            align_w: 0,
            mode: 0,
            pitch: 0.0,
            formant: 0.0,
            link: false,
            drive_db: 0.0,
            sat: crate::dsp::saturator::Core::default(),
            out: 1.0,
            period: 0.0,
            want: 0.0,
            shift: 0.0,
            shift_k: 1.0,
            last_m: 0.0,
            jump_t: 0.0,
            so: Smooth::default(),
            m: [0.0; 4],
            params: None,
            quiet: 0,
            dormant: true,
        }
    }
}

/// Semitones to shift a voice sung at MIDI pitch `m`.
fn voice_shift(mode: u32, pitch: f32, m: f32) -> f32 {
    let st = match mode {
        1 => m.round() - m + pitch,
        2 => ROBOT_NOTE + pitch.round() - m,
        _ => pitch,
    };
    st.clamp(-24.0, 24.0)
}

impl Voice {
    fn go_dormant(&mut self) {
        if let Some(d) = self.det.as_mut() {
            d.clear();
        }
        if let Some(p) = self.psola.as_mut() {
            p.clear();
        }
        self.align.fill(0.0);
        self.align_w = 0;
        self.period = 0.0;
        self.want = if self.mode == 0 { self.pitch } else { 0.0 };
        self.shift = self.want;
        self.last_m = 0.0;
        self.jump_t = 0.0;
        self.so.v = self.out;
        self.sat.park();
        self.m = [0.0; 4];
        self.quiet = 0;
        self.dormant = true;
    }
}

impl Processor for Voice {
    fn prepare(&mut self, sr: f32, _max: usize) {
        self.sr = sr;
        self.quiet = 0;
        self.dormant = true;
        self.det = Some(Detector::new(sr));
        self.psola = Some(Psola::new(sr));
        self.sat.set(&voice_sat(self.drive_db));
        self.sat.prepare(sr, 1);
        // the saturator's oversampling delay comes out of the look-ahead
        self.align = vec![0.0; align_delay(sr).saturating_sub(self.sat.latency())];
        self.align_w = 0;
        // fast enough to sound instant, slow enough that a snapped note doesn't click
        self.shift_k = 1.0 - (-1.0 / (sr * 0.004)).exp();
        self.so.setup(sr, 15.0);
        if let Some(p) = self.params {
            self.set_params(&p);
        }
    }
    fn set_params(&mut self, p: &ParamBlock) {
        let first = self.params.is_none();
        self.params = Some(*p);
        let v = p.values;
        self.mode = v[0].round().clamp(0.0, 2.0) as u32;
        self.pitch = v[1];
        self.formant = v[2];
        self.link = v[3] >= 0.5;
        self.drive_db = v[4].max(0.0);
        self.sat.set(&voice_sat(self.drive_db));
        self.out = db_to_gain(v[5]);
        if self.mode == 0 {
            self.want = self.pitch;
        }
        if first {
            self.shift = self.want;
            self.so.v = self.out;
        }
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _c: &ProcessCtx) {
        // Dormancy: digital silence for a second flushes everything the effect holds, so
        // it settles to one canonical state and does nothing until sound returns. A chain
        // that is skipped (`Processor::park`) is in exactly this state, so skipping it
        // changes nothing.
        let silent = l.iter().all(|&x| x == 0.0) && r.iter().all(|&x| x == 0.0);
        if silent {
            if self.dormant {
                return;
            }
            if self.quiet >= self.sr as usize {
                self.go_dormant();
                return;
            }
            self.quiet += l.len();
        } else {
            self.quiet = 0;
            self.dormant = false;
        }
        let (Some(det), Some(ps)) = (self.det.as_mut(), self.psola.as_mut()) else { return };
        let sr = self.sr;
        for i in 0..l.len() {
            let x = (l[i] + r[i]) * 0.5;
            if det.push(x) {
                // a lone estimate far from the line being sung is the detector grabbing a
                // harmonic: hold course until the jump has lasted 40 ms (see Tune)
                let mut glitch = false;
                if det.hz > 0.0 && det.confidence > 0.55 && self.last_m > 0.0 && (hz_to_midi(det.hz) - self.last_m).abs() > 5.0 {
                    self.jump_t += 0.004;
                    glitch = self.jump_t < 0.04;
                } else {
                    self.jump_t = 0.0;
                }
                if glitch {
                    // keep period and shift as they are
                } else if det.hz > 0.0 && det.confidence > 0.55 {
                    let m = hz_to_midi(det.hz);
                    self.last_m = m;
                    det.hint = det.hz;
                    self.period = sr / det.hz;
                    self.want = voice_shift(self.mode, self.pitch, m);
                    self.m = [det.hz, midi_to_hz(m + self.want), det.confidence, self.want];
                } else {
                    self.period = 0.0;
                    self.last_m = 0.0;
                    det.hint = 0.0;
                    self.m = [0.0, 0.0, det.confidence, 0.0];
                }
            }
            self.shift += (self.want - self.shift) * self.shift_k;
            let ratio = 2f32.powf(self.shift / 12.0);
            let formant = 2f32.powf((self.formant + if self.link { self.shift } else { 0.0 }) / 12.0);
            let n = self.align.len();
            let xd = if n > 0 {
                let d = self.align[self.align_w];
                self.align[self.align_w] = x;
                self.align_w = (self.align_w + 1) % n;
                d
            } else {
                x
            };
            l[i] = ps.process(xd, self.period, ratio, formant);
        }
        // drive after pitch and formant
        self.sat.process_mono(l);
        for i in 0..l.len() {
            let z = l[i] * self.so.next(self.out);
            l[i] = z;
            r[i] = z;
        }
    }
    fn reset(&mut self) {
        if let Some(p) = self.psola.as_mut() {
            p.reset();
        }
        self.align.fill(0.0);
        self.shift = self.want;
        self.sat.reset();
    }
    fn park(&mut self) {
        if !self.dormant {
            self.go_dormant();
        }
    }
    fn tail_samples(&self) -> Option<usize> {
        Some(self.align.len() + (self.sr * 1.5) as usize)
    }
    fn meters(&self, out: &mut [f32; METERS]) {
        out[..4].copy_from_slice(&self.m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{new_fx, param_block};
    use crate::model::FxKind;

    fn voice(hz: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (1..8).map(|k| (std::f32::consts::TAU * hz * k as f32 * i as f32 / sr).sin() / k as f32).sum::<f32>() * 0.3)
            .collect()
    }

    fn run(p: &mut dyn Processor, x: &[f32]) -> Vec<f32> {
        let ctx = ProcessCtx { sample_rate: 48000.0, ..Default::default() };
        let mut out = vec![];
        for c in x.chunks(256) {
            let mut l = c.to_vec();
            let mut r = c.to_vec();
            p.process(&mut l, &mut r, &ctx);
            out.extend(l);
        }
        out
    }

    #[test]
    fn tune_pulls_a_flat_note_to_the_scale() {
        let sr = 48000.0;
        let mut fx = new_fx(1, FxKind::Tune, 120.0);
        for (k, v) in [("retune", 0.0), ("flex", 0.0), ("vibrato", 0.0), ("humanize", 0.0)] {
            fx.params.insert(k.into(), v);
        }
        let mut p = Tune::default();
        p.prepare(sr, 256);
        p.set_params(&param_block(&fx));
        // A3 (220 Hz) sung 40 cents flat, chromatic scale → corrected to 220
        let x = voice(220.0 * 2f32.powf(-0.4 / 12.0), sr, 48000);
        let y = run(&mut p, &x);
        let mut d = Detector::new(sr);
        for &s in &y[24000..] {
            d.push(s);
        }
        let cents = 1200.0 * (d.hz / 220.0).log2();
        assert!(cents.abs() < 12.0, "still {cents:.1} cents off");
        let mut m = [0f32; METERS];
        p.meters(&mut m);
        assert!((m[1] - 220.0).abs() < 1.0, "target {}", m[1]);
    }

    #[test]
    fn tune_corrects_a_phone_like_low_male_voice() {
        // ~123 Hz, almost no fundamental, loud 3rd–5th harmonics (formant), sung 40 cents sharp
        let sr = 44100.0;
        for (f0, target) in [(123.47f32 * 2f32.powf(0.4 / 12.0), 123.47f32), (146.83 * 2f32.powf(-0.35 / 12.0), 146.83)] {
            let mut fx = new_fx(1, FxKind::Tune, 120.0);
            for (k, v) in [("retune", 0.0), ("flex", 0.0), ("vibrato", 0.0), ("humanize", 0.0), ("voice", 2.0)] {
                fx.params.insert(k.into(), v);
            }
            let mut p = Tune::default();
            p.prepare(sr, 256);
            p.set_params(&param_block(&fx));
            let amps = [0.03f32, 0.15, 0.9, 1.0, 0.7, 0.3, 0.2, 0.1, 0.08, 0.05];
            let x: Vec<f32> = (0..44100)
                .map(|i| {
                    amps.iter()
                        .enumerate()
                        .map(|(k, a)| a * (std::f32::consts::TAU * f0 * (k + 1) as f32 * i as f32 / sr).sin())
                        .sum::<f32>()
                        * 0.08
                })
                .collect();
            let y = run(&mut p, &x);
            let mut d = Detector::new(sr);
            for &s in &y[22050..] {
                d.push(s);
            }
            let cents = 1200.0 * (d.hz / target).log2();
            assert!(cents.abs() < 12.0, "{f0:.1} Hz → {:.1} Hz, still {cents:.1} cents off {target}", d.hz);
        }
    }

    #[test]
    fn natural_mode_leaves_a_slide_alone_then_settles() {
        // 0.4 s at A3 (in tune), 0.3 s slide up to ~C4 + 30 cents, then held
        let sr = 48000.0;
        let mut fx = new_fx(1, FxKind::Tune, 120.0);
        for (k, v) in [("retune", 40.0), ("flex", 0.0), ("vibrato", 0.0), ("humanize", 0.0)] {
            fx.params.insert(k.into(), v);
        }
        let mut p = Tune::default();
        p.prepare(sr, 256);
        p.set_params(&param_block(&fx));
        let (a, b) = (57.0f32, 60.3f32);
        let n = (sr * 1.4) as usize;
        let mut ph = 0f32;
        let mut pitch = vec![0f32; n];
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                let m = if t < 0.4 {
                    a
                } else if t < 0.7 {
                    a + (b - a) * (t - 0.4) / 0.3
                } else {
                    b
                };
                pitch[i] = m;
                ph += std::f32::consts::TAU * midi_to_hz(m) / sr;
                (1..6).map(|k| (ph * k as f32).sin() / k as f32).sum::<f32>() * 0.3
            })
            .collect();
        let y = run(&mut p, &x);
        let lat = tune_latency(sr);
        let measure = |from: f32, to: f32| {
            let mut d = Detector::new(sr);
            let (i0, i1) = ((from * sr) as usize + lat, (to * sr) as usize + lat);
            for &s in &y[i0..i1.min(y.len())] {
                d.push(s);
            }
            hz_to_midi(d.hz)
        };
        // mid-slide: the output still travels with the input (no snapping to a note on the way)
        let mid = measure(0.45, 0.58);
        let input_mid = pitch[(0.58 * sr) as usize - Detector::new(sr).latency()];
        assert!((mid - input_mid).abs() < 0.45, "mid-slide output {mid:.2} vs input {input_mid:.2}");
        // settled: pulled to C4
        let end = measure(1.0, 1.35);
        assert!((end - 60.0).abs() < 0.12, "held note ended at {end:.2}");
    }

    #[test]
    fn centre_correction_keeps_vibrato_and_moves_the_note() {
        // A3 + 30 cents with a 5.5 Hz ±40-cent vibrato; vibrato = 100 % → the note centre
        // lands on A3 and the vibrato survives
        let sr = 48000.0;
        let mut fx = new_fx(1, FxKind::Tune, 120.0);
        for (k, v) in [("retune", 10.0), ("flex", 0.0), ("vibrato", 100.0), ("humanize", 0.0)] {
            fx.params.insert(k.into(), v);
        }
        let mut p = Tune::default();
        p.prepare(sr, 256);
        p.set_params(&param_block(&fx));
        let n = (sr * 2.0) as usize;
        let mut ph = 0f32;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                let m = 57.3 + 0.4 * (std::f32::consts::TAU * 5.5 * t).sin();
                ph += std::f32::consts::TAU * midi_to_hz(m) / sr;
                (1..6).map(|k| (ph * k as f32).sin() / k as f32).sum::<f32>() * 0.3
            })
            .collect();
        let y = run(&mut p, &x);
        let lat = tune_latency(sr);
        let mut d = Detector::new(sr);
        let mut track = vec![];
        for &s in &y[(0.6 * sr) as usize + lat..(1.9 * sr) as usize + lat] {
            if d.push(s) && d.hz > 0.0 {
                track.push(hz_to_midi(d.hz));
            }
        }
        let mean = track.iter().sum::<f32>() / track.len() as f32;
        let (lo, hi) = track.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        assert!((mean - 57.0).abs() < 0.1, "note centre at {mean:.2}");
        assert!(hi - lo > 0.5, "vibrato flattened to {:.2} st", hi - lo);
    }

    #[test]
    fn deesser_ducks_sibilance_not_vowels() {
        let sr = 48000.0;
        let mk = || {
            let mut d = DeEss::default();
            d.prepare(sr, 256);
            d.set_params(&param_block(&new_fx(1, FxKind::Deess, 120.0)));
            d
        };
        // vowel: 220 Hz harmonics, loud
        let vowel = voice(220.0, sr, 24000);
        let mut d = mk();
        run(&mut d, &vowel);
        assert!(d.gain_reduction_db() > -1.0, "vowel ducked {}", d.gain_reduction_db());
        // "s": band-limited noise around 7 kHz
        let mut seed = 3u32;
        let mut bp = Biquad::default();
        bp.set(FilterType::Bandpass, sr, 7500.0, 1.0, 0.0);
        let s: Vec<f32> = (0..24000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                bp.tick((seed as f32 / u32::MAX as f32 - 0.5) * 1.6)
            })
            .collect();
        let mut d = mk();
        run(&mut d, &s);
        assert!(d.gain_reduction_db() < -3.0, "sibilant not ducked: {}", d.gain_reduction_db());
    }

    #[test]
    fn fet_compresses_harder_with_more_input() {
        let sr = 48000.0;
        let x = voice(220.0, sr, 24000);
        let gr = |input: f64| {
            let mut fx = new_fx(1, FxKind::Fet, 120.0);
            fx.params.insert("input".into(), input);
            let mut p = Fet::default();
            p.prepare(sr, 256);
            p.set_params(&param_block(&fx));
            run(&mut p, &x);
            p.gain_reduction_db()
        };
        let (a, b) = (gr(0.0), gr(24.0));
        assert!(b < a - 6.0, "input 0 dB → {a}, 24 dB → {b}");
    }

    fn voice_fx(sr: f32, kv: &[(&str, f64)]) -> Voice {
        let mut fx = new_fx(1, FxKind::Voice, 120.0);
        for &(k, v) in kv {
            fx.params.insert(k.into(), v);
        }
        let mut p = Voice::default();
        p.prepare(sr, 256);
        p.set_params(&param_block(&fx));
        p
    }

    /// Mean detected pitch (MIDI) of `y` from `from` seconds of input time on.
    fn sung(y: &[f32], sr: f32, from: f32) -> f32 {
        let mut d = Detector::new(sr);
        let mut track = vec![];
        for &s in &y[(from * sr) as usize + tune_latency(sr)..] {
            if d.push(s) && d.hz > 0.0 && d.confidence > 0.6 {
                track.push(hz_to_midi(d.hz));
            }
        }
        assert!(!track.is_empty(), "no pitch in the output");
        track.iter().sum::<f32>() / track.len() as f32
    }

    #[test]
    fn voice_at_defaults_is_the_input_delayed() {
        let sr = 48000.0;
        let x = voice(196.0, sr, 48000);
        let y = run(&mut voice_fx(sr, &[]), &x);
        let lat = tune_latency(sr);
        let (mut err, mut sig) = (0f32, 0f32);
        for i in 12000..x.len() - lat {
            err += (y[i + lat] - x[i]).powi(2);
            sig += x[i].powi(2);
        }
        let rel = (err / sig).sqrt();
        assert!(rel < 0.05, "default Voice colours the signal: {rel:.3}");
    }

    #[test]
    fn voice_transposes_quantizes_and_robots() {
        let sr = 48000.0;
        // A3 sung 35 cents flat
        let x = voice(220.0 * 2f32.powf(-0.35 / 12.0), sr, 48000);
        let m_in = 57.0 - 0.35;
        let t = sung(&run(&mut voice_fx(sr, &[("pitch", 5.0)]), &x), sr, 0.4);
        assert!((t - (m_in + 5.0)).abs() < 0.12, "transpose +5 landed at {t:.2}");
        let q = sung(&run(&mut voice_fx(sr, &[("mode", 1.0), ("pitch", -12.0)]), &x), sr, 0.4);
        assert!((q - 45.0).abs() < 0.12, "quantize −12 landed at {q:.2}");
        // robot: two different sung notes both come out on C4 + 2 = D4
        for hz in [220.0f32, 164.81] {
            let r = sung(&run(&mut voice_fx(sr, &[("mode", 2.0), ("pitch", 2.0)]), &voice(hz, sr, 48000)), sr, 0.4);
            assert!((r - 62.0).abs() < 0.12, "robot from {hz} Hz landed at {r:.2}");
        }
    }

    #[test]
    fn voice_drive_adds_harmonics_only_when_turned_up() {
        let sr = 48000.0;
        // a pure 220 Hz tone: anything at 440 Hz in the output is distortion
        let x: Vec<f32> = (0..48000).map(|i| (std::f32::consts::TAU * 220.0 * i as f32 / sr).sin() * 0.5).collect();
        let h2 = |y: &[f32]| {
            let seg = &y[24000..];
            let at = |f: f32| {
                let (mut re, mut im) = (0f32, 0f32);
                for (i, &s) in seg.iter().enumerate() {
                    let ph = std::f32::consts::TAU * f * i as f32 / sr;
                    re += s * ph.cos();
                    im += s * ph.sin();
                }
                (re * re + im * im).sqrt()
            };
            at(440.0) / at(220.0)
        };
        let clean = h2(&run(&mut voice_fx(sr, &[]), &x));
        let driven = h2(&run(&mut voice_fx(sr, &[("drive", 18.0)]), &x));
        assert!(clean < 0.01, "drive 0 is not clean: H2 {clean:.4}");
        assert!(driven > 0.03, "drive 18 dB adds no even harmonics: H2 {driven:.4}");
    }
}
