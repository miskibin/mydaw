//! Warp: plays the input back through a drawn **time map** and **volume map** over a two-bar
//! buffer, locked to the song's bars — half-time, stutters, rolls, tape stops, reverses and
//! scratches (time), gates and sidechain pumps (volume).
//!
//! ```text
//!  in ─┬─ ring (2 bars, absolute sample index) ─ read head(s) at now − Y(x) ─┐
//!      │                                                                    ├─ Time mix ─ × gain ─ (DC) ─ out
//!      └─ 2-sample delay ──────────────────── dry ───────────────────────────┘     ↑
//!                                               volume map at x ─ ATT/REL ─ Volume mix
//! ```
//!
//! * `x` = position in the bar, beats 0..4: the song's bar phase (Position: Song) or the beats
//!   since the slot started (Position: Trigger). It comes from `ProcessCtx::beat` per sample,
//!   never from a free-running counter.
//! * A time map's value `v` is how far back the head reads: `Y = (1 − v) · 8` beats. The read
//!   position is `now − Y`, so a flat line plays at normal speed, a slope of −1 beat/beat stops
//!   (the "safety line"), −2 plays backwards at normal speed, a vertical step jumps.
//! * The buffer only holds what was heard since playback started or the playhead last jumped:
//!   a map that reaches further back is silent there (the safety line), never stale audio.
//! * Click reduction: a jump of the read position crossfades (linearly) from the old trajectory,
//!   kept running, to the new one over 3 ms. Attack protect shortens it to 0.5 ms, so a jump
//!   onto a drum hit keeps its attack.
//! * Slots change when their parameter changes (automation with Hold points, or a click):
//!   at once, or at the next 1/16 … bar (Trigger sync; a change that arrives within 1/32 beat
//!   after a boundary counts as on it). One-shot plays a slot for one bar, then slot 1.
//! * Hold: Last bar loops the bar before the current one, Next bar records the coming bar
//!   and loops it; the buffer stops writing while it loops (so the bar survives), the volume
//!   map keeps playing. After a hold the buffer starts empty again.
//! * The maps arrive through `Processor::swap_data` (built off the audio thread). 2 samples of
//!   latency (the interpolator reads around the position): the dry path is delayed to match
//!   and the latency is reported for compensation.
//! * Stopped transport: the effect passes the input through.

use std::any::Any;

use crate::model::{WarpMaps, WarpSlot, WARP_BAR, WARP_DEPTH};
use crate::processor::{FxData, ParamBlock, ProcessCtx, Processor, METERS};

/// Order of the parameter list in `catalog::params(FxKind::Warp)`.
pub mod idx {
    pub const TSLOT: usize = 0;
    pub const VSLOT: usize = 1;
    pub const TRIG: usize = 2;
    pub const POS: usize = 3;
    pub const ONESHOT: usize = 4;
    pub const HOLD: usize = 5;
    pub const TMIX: usize = 6;
    pub const VMIX: usize = 7;
    pub const ATT: usize = 8;
    pub const REL: usize = 9;
    pub const TNS: usize = 10;
    pub const CLICK: usize = 11;
    pub const ATTACK: usize = 12;
    pub const DC: usize = 13;
    pub const COUNT: usize = 14;
}

pub const SLOT_NAMES: [&str; 16] = [
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
];
pub const TRIGS: [&str; 6] = ["Off", "1/16", "1/8", "1/4", "1/2", "1 takt"];
pub const TRIG_BEATS: [f64; 6] = [0.0, 0.25, 0.5, 1.0, 2.0, 4.0];
pub const POSITIONS: [&str; 2] = ["Song", "Trigger"];
pub const HOLDS: [&str; 3] = ["Off", "Poprzedni takt", "Następny takt"];

/// Samples of latency (the Hermite read needs two samples after the position).
pub const LATENCY: usize = 2;
/// The buffer holds this many seconds: two bars down to 40 bpm.
pub const BUFFER_S: f32 = 12.5;
/// A slot change arriving this soon after a sync boundary counts as on it, beats.
const TRIG_TOL: f64 = 1.0 / 32.0;
/// Click-reduction crossfade, and the shorter one with Attack protect, seconds.
const XF_S: f32 = 0.003;
const XF_ATTACK_S: f32 = 0.0005;
/// A read position this many samples off its own trajectory is a jump.
const JUMP: f64 = 4.0;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum Hold {
    #[default]
    Off,
    /// waiting for the bar that starts at this beat to be recorded
    Recording { until: f64 },
    /// looping `len` samples from absolute `start`
    Looping { start: f64, len: f64 },
}

#[derive(Default)]
struct Bank {
    active: usize,
    pending: Option<(usize, f64)>,
    param: usize,
    start_beat: f64,
    oneshot_end: Option<f64>,
}

impl Bank {
    /// Position in the bar for this bank at `beat`.
    fn x(&self, beat: f64, song: bool) -> f64 {
        let b = if song { beat } else { beat - self.start_beat };
        b.rem_euclid(WARP_BAR)
    }
    fn activate(&mut self, slot: usize, beat: f64, oneshot: bool) {
        self.active = slot;
        self.start_beat = beat;
        self.oneshot_end = if oneshot && slot != 0 {
            Some(beat + WARP_BAR)
        } else {
            None
        };
    }
    /// A new parameter value: switch now or at the next boundary of `q` beats.
    fn request(&mut self, slot: usize, beat: f64, q: f64, oneshot: bool) {
        self.param = slot;
        if q <= 0.0 {
            self.pending = None;
            self.activate(slot, beat, oneshot);
            return;
        }
        let floor = (beat / q).floor() * q;
        let at = if beat - floor < TRIG_TOL {
            floor
        } else {
            floor + q
        };
        self.pending = Some((slot, at));
    }
    /// Advance to `beat`: apply a due pending change or end a one-shot.
    fn tick(&mut self, beat: f64, oneshot: bool) {
        if let Some((slot, at)) = self.pending {
            if beat >= at {
                self.pending = None;
                self.activate(slot, at, oneshot);
            }
        }
        if let Some(end) = self.oneshot_end {
            if beat >= end {
                self.oneshot_end = None;
                self.active = 0;
                self.start_beat = end;
            }
        }
    }
}

#[derive(Default)]
pub struct Warp {
    sr: f32,
    p: [f32; idx::COUNT],
    have: bool,
    maps: Option<Box<WarpMaps>>,
    ring: [Vec<f32>; 2],
    /// Absolute index of the next sample written.
    abs: u64,
    /// Oldest absolute index that belongs to the current, contiguous history.
    valid_from: u64,
    frozen: bool,
    dry: [[f32; LATENCY]; 2],
    tb: Bank,
    vb: Bank,
    hold: Hold,
    hold_param: usize,
    /// read position (absolute samples) of the previous sample and its rate
    pos_prev: f64,
    rate_prev: f64,
    has_prev: bool,
    // crossfade from the old trajectory
    old_pos: f64,
    old_rate: f64,
    xf: f32,
    gate: f32,
    gain: f32,
    dc: [(f32, f32); 2],
    next_beat: f64,
    was_playing: bool,
    // meters
    m_x: f32,
    m_y: f32,
    m_speed: f32,
    m_gain: f32,
}

#[inline]
fn coef(sr: f32, ms: f32) -> f32 {
    if ms <= 0.0 {
        1.0
    } else {
        1.0 - (-1.0 / (sr * ms / 1000.0)).exp()
    }
}

impl Warp {
    fn choice(&self, i: usize, n: usize) -> usize {
        (self.p[i].round().max(0.0) as usize).min(n - 1)
    }

    fn slot(maps: &Option<Box<WarpMaps>>, time: bool, i: usize) -> Option<&WarpSlot> {
        let m = maps.as_ref()?;
        if time {
            m.time.get(i)
        } else {
            m.vol.get(i)
        }
    }

    /// Hermite read at absolute position `pos`; 0 outside the valid history.
    #[inline]
    fn read(&self, c: usize, pos: f64) -> f32 {
        let buf = &self.ring[c];
        let n = buf.len() as u64;
        if n < 8
            || pos < self.valid_from as f64 + 1.0
            || pos > self.abs as f64 - 1.0 - LATENCY as f64
            || pos < (self.abs as f64 - n as f64 + 2.0)
        {
            return 0.0;
        }
        let i = pos.floor();
        let f = (pos - i) as f32;
        let i = i as u64;
        let at = |k: u64| buf[(k % n) as usize];
        let (xm1, x0, x1, x2) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * f + c2) * f + c1) * f + x0
    }

    #[inline]
    fn in_history(&self, pos: f64) -> bool {
        let n = self.ring[0].len() as f64;
        pos >= self.valid_from as f64 + 1.0
            && pos <= self.abs as f64 - 1.0 - LATENCY as f64
            && pos >= self.abs as f64 - n + 2.0
    }

    /// Forget the history (playhead jump, start, end of a hold).
    fn forget(&mut self) {
        self.valid_from = self.abs;
        self.has_prev = false;
        self.xf = 0.0;
    }

    fn snap(&mut self) {
        self.tb.param = self.choice(idx::TSLOT, 16);
        self.vb.param = self.choice(idx::VSLOT, 16);
        self.tb.active = self.tb.param;
        self.vb.active = self.vb.param;
        self.tb.pending = None;
        self.vb.pending = None;
        self.hold_param = self.choice(idx::HOLD, 3);
    }
}

impl Processor for Warp {
    fn prepare(&mut self, sr: f32, _max_block: usize) {
        self.sr = sr;
        let n = (BUFFER_S * sr) as usize + 8;
        self.ring = [vec![0.0; n], vec![0.0; n]];
        self.abs = 0;
        self.valid_from = 0;
        self.gain = 1.0;
        self.gate = 0.0;
    }

    fn set_params(&mut self, pb: &ParamBlock) {
        let first = !self.have;
        self.p.copy_from_slice(&pb.values[..idx::COUNT]);
        self.have = true;
        if first {
            self.snap();
            return;
        }
        let beat = self.next_beat;
        let q = TRIG_BEATS[self.choice(idx::TRIG, TRIG_BEATS.len())];
        let one = self.p[idx::ONESHOT] >= 0.5;
        let t = self.choice(idx::TSLOT, 16);
        if t != self.tb.param {
            self.tb.request(t, beat, q, one);
        }
        let v = self.choice(idx::VSLOT, 16);
        if v != self.vb.param {
            self.vb.request(v, beat, q, one);
        }
    }

    fn swap_data(&mut self, data: FxData) -> Option<FxData> {
        match data.downcast::<WarpMaps>() {
            Ok(maps) => self.maps.replace(maps).map(|m| m as Box<dyn Any + Send>),
            Err(other) => Some(other),
        }
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &ProcessCtx) {
        let n = self.ring[0].len();
        if n == 0 || !self.have {
            return;
        }
        let sr = self.sr;
        let bpm = if ctx.bpm > 1.0 { ctx.bpm } else { 120.0 };
        let bps = bpm / 60.0 / sr as f64;
        let spb = sr as f64 * 60.0 / bpm;
        if !ctx.playing {
            // pass through (delayed like the wet path), nothing is recorded
            for i in 0..l.len() {
                for (c, x) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                    let d = &mut self.dry[c];
                    let y = d[0];
                    d.copy_within(1.., 0);
                    d[LATENCY - 1] = *x;
                    *x = y;
                }
            }
            self.was_playing = false;
            self.m_x = -1.0;
            return;
        }
        // a playhead jump (or a start) makes the buffer's past unrelated to the song here
        if !self.was_playing || (ctx.beat - self.next_beat).abs() > 0.01 {
            self.forget();
            if self.frozen {
                self.frozen = false;
            }
            self.hold = Hold::Off;
            self.hold_param = usize::MAX; // re-arm a hold that is on
            if !self.was_playing {
                // slots start where the song is
                let (t, v) = (self.choice(idx::TSLOT, 16), self.choice(idx::VSLOT, 16));
                self.tb.param = t;
                self.vb.param = v;
                self.tb.pending = None;
                self.vb.pending = None;
                self.tb.activate(t, ctx.beat, false);
                self.vb.activate(v, ctx.beat, false);
            }
        }
        self.was_playing = true;
        let song = self.choice(idx::POS, 2) == 0;
        let oneshot = self.p[idx::ONESHOT] >= 0.5;
        let tmix = (self.p[idx::TMIX] / 100.0).clamp(0.0, 1.0);
        let vmix = (self.p[idx::VMIX] / 100.0).clamp(0.0, 1.0);
        let tns = (self.p[idx::TNS] / 100.0).clamp(-1.0, 1.0);
        let click = self.p[idx::CLICK] >= 0.5;
        let floor_ms = if click { 1.0 } else { 0.0 };
        let k_up = coef(sr, self.p[idx::ATT].max(floor_ms));
        let k_dn = coef(sr, self.p[idx::REL].max(floor_ms));
        let xf_len = (if self.p[idx::ATTACK] >= 0.5 {
            XF_ATTACK_S
        } else {
            XF_S
        } * sr)
            .max(1.0);
        let k_gate = coef(sr, 1.0);
        let dc_on = self.p[idx::DC] >= 0.5;
        let dc_r = 1.0 - std::f32::consts::TAU * 5.0 / sr;
        let ring_len = n as u64;

        // hold: a change of the parameter starts or ends it
        let hp = self.choice(idx::HOLD, 3);
        if hp != self.hold_param {
            self.hold_param = hp;
            match hp {
                0 => {
                    if self.frozen {
                        self.frozen = false;
                        self.forget();
                    }
                    self.hold = Hold::Off;
                }
                1 => {
                    // the bar before the current one, if it was heard
                    let bar0 = (ctx.beat / WARP_BAR).floor() * WARP_BAR;
                    let start = self.abs as f64 - 1.0 - (ctx.beat - (bar0 - WARP_BAR)) * spb;
                    let len = WARP_BAR * spb;
                    if self.in_history(start) {
                        self.frozen = true;
                        self.hold = Hold::Looping { start, len };
                    } else {
                        self.hold = Hold::Recording {
                            until: bar0 + WARP_BAR + WARP_BAR,
                        };
                    }
                }
                _ => {
                    let bar1 = (ctx.beat / WARP_BAR).ceil() * WARP_BAR;
                    self.hold = Hold::Recording {
                        until: bar1 + WARP_BAR,
                    };
                }
            }
        }

        let maps = self.maps.take();
        for i in 0..l.len() {
            let beat = ctx.beat + i as f64 * bps;
            self.tb.tick(beat, oneshot);
            self.vb.tick(beat, oneshot);
            let (xl, xr) = (l[i], r[i]);
            // record
            if !self.frozen {
                let w = (self.abs % ring_len) as usize;
                self.ring[0][w] = xl;
                self.ring[1][w] = xr;
                self.abs += 1;
            }
            if let Hold::Recording { until } = self.hold {
                if beat >= until {
                    let len = WARP_BAR * spb;
                    let start = self.abs as f64 - 1.0 - len;
                    if self.in_history(start) {
                        self.frozen = true;
                        self.hold = Hold::Looping { start, len };
                    } else {
                        self.hold = Hold::Recording {
                            until: until + WARP_BAR,
                        };
                    }
                }
            }
            // where the head reads
            let xt = self.tb.x(beat, song);
            let (pos, y_beats) = match self.hold {
                Hold::Looping { start, len } => (start + (beat / WARP_BAR).fract() * len, 0.0),
                _ => {
                    let v = Self::slot(&maps, true, self.tb.active)
                        .map(|s| s.value_at(xt, 0.0))
                        .unwrap_or(1.0)
                        .clamp(0.0, 1.0);
                    let y = (1.0 - v as f64) * WARP_DEPTH;
                    let d = (y * spb).max(LATENCY as f64);
                    (self.abs as f64 - 1.0 - d, y)
                }
            };
            // jumps → crossfade from the old trajectory
            if self.has_prev {
                let expect = self.pos_prev + self.rate_prev;
                if (pos - expect).abs() > JUMP {
                    // a jump: the speed carries over (the jump itself is not a rate)
                    if click {
                        self.old_pos = expect;
                        self.old_rate = self.rate_prev;
                        self.xf = 1.0;
                    }
                } else {
                    self.rate_prev = pos - self.pos_prev;
                }
            } else {
                self.rate_prev = 1.0;
            }
            self.pos_prev = pos;
            self.has_prev = true;
            let valid = if self.in_history(pos) { 1.0 } else { 0.0 };
            self.gate += (valid - self.gate) * k_gate;
            let (mut wl, mut wr) = (self.read(0, pos) * self.gate, self.read(1, pos) * self.gate);
            if self.xf > 0.0 {
                // linear: the two heads often read the same material (a loop wrap, a stutter of
                // a held note), where an equal-power fade would bump the level by 3 dB
                self.xf = (self.xf - 1.0 / xf_len).max(0.0);
                let (gi, go) = (1.0 - self.xf, self.xf);
                wl = wl * gi + self.read(0, self.old_pos) * go;
                wr = wr * gi + self.read(1, self.old_pos) * go;
                self.old_pos += self.old_rate;
            }
            // dry, delayed to line up
            let mut dry = [0.0; 2];
            for (c, x) in [xl, xr].into_iter().enumerate() {
                let d = &mut self.dry[c];
                dry[c] = d[0];
                d.copy_within(1.., 0);
                d[LATENCY - 1] = x;
            }
            // volume map
            let xv = self.vb.x(beat, song);
            let g_t = Self::slot(&maps, false, self.vb.active)
                .map(|s| s.value_at(xv, tns))
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);
            let k = if g_t > self.gain { k_up } else { k_dn };
            self.gain += (g_t - self.gain) * k;
            let g = 1.0 + (self.gain - 1.0) * vmix;
            let mut ol = (dry[0] + (wl - dry[0]) * tmix) * g;
            let mut or = (dry[1] + (wr - dry[1]) * tmix) * g;
            if dc_on {
                for (c, o) in [&mut ol, &mut or].into_iter().enumerate() {
                    let (x1, y1) = self.dc[c];
                    let y = *o - x1 + dc_r * y1;
                    self.dc[c] = (*o, if y.abs() < 1e-20 { 0.0 } else { y });
                    *o = y;
                }
            }
            l[i] = if ol.is_finite() { ol } else { 0.0 };
            r[i] = if or.is_finite() { or } else { 0.0 };
            self.m_x = xt as f32;
            self.m_y = -(y_beats as f32);
            self.m_speed = self.rate_prev as f32;
            self.m_gain = self.gain;
        }
        self.maps = maps;
        self.next_beat = ctx.beat + l.len() as f64 * bps;
    }

    fn reset(&mut self) {
        for b in self.ring.iter_mut() {
            b.fill(0.0);
        }
        self.dry = [[0.0; LATENCY]; 2];
        self.dc = Default::default();
        self.frozen = false;
        self.hold = Hold::Off;
        self.hold_param = usize::MAX;
        self.forget();
        self.was_playing = false;
        self.gain = 1.0;
        self.gate = 0.0;
        if self.have {
            self.snap();
            self.hold_param = usize::MAX;
        }
    }

    fn tail_samples(&self) -> Option<usize> {
        // anything in the buffer can come back (a map reads up to two bars back, a hold loops)
        if self.frozen {
            None
        } else {
            Some(self.ring[0].len() + LATENCY)
        }
    }

    fn meters(&self, out: &mut [f32; METERS]) {
        out[0] = self.m_x;
        out[1] = self.tb.active as f32;
        out[2] = self.vb.active as f32;
        out[3] = self.tb.pending.map(|p| p.0 as f32).unwrap_or(-1.0);
        out[4] = self.vb.pending.map(|p| p.0 as f32).unwrap_or(-1.0);
        out[5] = self.m_y;
        out[6] = self.m_speed;
        out[7] = self.m_gain;
        let filled = (self.abs - self.valid_from).min(self.ring[0].len() as u64) as f32;
        let spb = if self.sr > 0.0 {
            self.sr * 60.0 / 120.0
        } else {
            1.0
        };
        out[8] = filled / spb; // in beats at 120 bpm; the UI rescales with the song tempo
        out[9] = match self.hold {
            Hold::Off => 0.0,
            Hold::Recording { .. } => 1.0,
            Hold::Looping { .. } => 2.0,
        };
        out[10] = filled / self.sr.max(1.0);
    }
}

/// The factory slots (also the catalog's `warp` section): `src/warp_factory.json`.
pub fn factory_maps() -> WarpMaps {
    #[derive(serde::Deserialize)]
    struct File {
        time: Vec<WarpSlot>,
        vol: Vec<WarpSlot>,
    }
    let f: File = serde_json::from_str(include_str!("../warp_factory.json"))
        .expect("warp_factory.json is valid");
    WarpMaps {
        time: f.time,
        vol: f.vol,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AutoPoint, CurveShape};

    const SR: f32 = 48000.0;

    fn params(kv: &[(usize, f32)]) -> ParamBlock {
        let mut pb = ParamBlock::default();
        for (i, d) in crate::catalog::params(crate::model::FxKind::Warp)
            .iter()
            .enumerate()
        {
            pb.values[i] = d.def as f32;
        }
        // test signals are often DC: the DC filter would take them away
        pb.values[idx::DC] = 0.0;
        for &(i, v) in kv {
            pb.values[i] = v;
        }
        pb
    }

    fn with_maps(time: Vec<AutoPoint>, vol: Vec<AutoPoint>) -> Box<WarpMaps> {
        Box::new(WarpMaps {
            time: vec![WarpSlot {
                name: "t".into(),
                points: time,
            }],
            vol: vec![WarpSlot {
                name: "v".into(),
                points: vol,
            }],
        })
    }

    fn flat() -> Vec<AutoPoint> {
        vec![AutoPoint::at(0.0, 1.0), AutoPoint::at(4.0, 1.0)]
    }

    /// Runs `secs` of a 440 Hz sine (or `input`) at 120 bpm from beat 0; returns the left output.
    fn run(w: &mut Warp, input: impl Fn(usize) -> f32, n: usize) -> Vec<f32> {
        let bps = 120.0 / 60.0 / SR as f64;
        let mut out = vec![];
        let mut i = 0;
        while i < n {
            let m = 256.min(n - i);
            let mut l: Vec<f32> = (i..i + m).map(&input).collect();
            let mut r = l.clone();
            w.process(
                &mut l,
                &mut r,
                &ProcessCtx {
                    sample_rate: SR,
                    bpm: 120.0,
                    beat: i as f64 * bps,
                    playing: true,
                },
            );
            out.extend_from_slice(&l);
            i += m;
        }
        out
    }

    fn make(time: Vec<AutoPoint>, vol: Vec<AutoPoint>, kv: &[(usize, f32)]) -> Warp {
        let mut w = Warp::default();
        w.prepare(SR, 256);
        w.set_params(&params(kv));
        assert!(w.swap_data(with_maps(time, vol)).is_none());
        w
    }

    fn freq(x: &[f32]) -> f32 {
        let mut zc = vec![];
        for i in 1..x.len() {
            if x[i - 1] < 0.0 && x[i] >= 0.0 {
                zc.push(i as f32 - x[i] / (x[i] - x[i - 1]));
            }
        }
        if zc.len() < 3 {
            return 0.0;
        }
        (zc.len() - 1) as f32 * SR / (zc[zc.len() - 1] - zc[0])
    }

    const SPB: usize = 24000; // samples per beat at 120 bpm

    #[test]
    fn flat_map_is_the_input_delayed_by_the_latency() {
        let mut w = make(flat(), flat(), &[]);
        let sine = |i: usize| (i as f32 * 0.05).sin() * 0.5;
        let out = run(&mut w, sine, 48000);
        for i in 1000..48000 {
            assert!(
                (out[i] - sine(i - LATENCY)).abs() < 2e-3,
                "sample {i}: {} vs {}",
                out[i],
                sine(i - LATENCY)
            );
        }
    }

    #[test]
    fn half_time_slope_plays_an_octave_down() {
        // v from 1 to 0.75 over the bar: two beats behind at the end → half speed
        let mut w = make(
            vec![AutoPoint::at(0.0, 1.0), AutoPoint::at(4.0, 0.75)],
            flat(),
            &[],
        );
        let out = run(
            &mut w,
            |i| (i as f32 * 440.0 * std::f32::consts::TAU / SR).sin(),
            SPB * 4,
        );
        let f = freq(&out[SPB..SPB * 3]);
        assert!((f - 220.0).abs() < 1.0, "half speed: {f} Hz");
    }

    #[test]
    fn reverse_slope_plays_the_past_backwards() {
        // slope −2 beats per beat over beat 2..3 (v drops 2/8): reads backwards at normal speed
        let pts = vec![
            AutoPoint::at(0.0, 1.0),
            AutoPoint::at(2.0, 1.0),
            AutoPoint::at(3.0, 0.75),
            AutoPoint::at(3.0, 1.0),
            AutoPoint::at(4.0, 1.0),
        ];
        let mut w = make(pts, flat(), &[(idx::CLICK, 0.0)]);
        // a rising ramp as input: reversed it falls
        let out = run(&mut w, |i| (i % SPB) as f32 / SPB as f32 - 0.5, SPB * 4);
        let seg = &out[SPB * 2 + 100..SPB * 3 - 100];
        let falling = seg.windows(2).filter(|w| w[1] < w[0]).count();
        assert!(
            falling > seg.len() * 9 / 10,
            "reverse: {falling} of {} samples fall",
            seg.len()
        );
    }

    #[test]
    fn stutter_repeats_the_first_beat() {
        // Y = −floor(x): every beat replays beat 1 (hold steps)
        let s = factory_maps()
            .time
            .into_iter()
            .find(|s| s.name.starts_with("Stutter — 1."))
            .unwrap();
        let mut w = make(s.points, flat(), &[]);
        let input = |i: usize| ((i / 4800) % 7) as f32 * 0.1; // a staircase that never repeats within a beat
        let out = run(&mut w, input, SPB * 4);
        for k in 1..4 {
            for j in [500usize, 6000, 15000] {
                let a = out[j];
                let b = out[k * SPB + j];
                assert!(
                    (a - b).abs() < 1e-3,
                    "beat {k} sample {j}: {b} should repeat {a}"
                );
            }
        }
    }

    #[test]
    fn reaching_before_the_recorded_past_is_silent() {
        // from the very start, read 4 beats back: nothing was heard yet → silence, not garbage
        let mut w = make(
            vec![AutoPoint::at(0.0, 0.5), AutoPoint::at(4.0, 0.5)],
            flat(),
            &[],
        );
        let out = run(&mut w, |_| 0.5, SPB * 6);
        assert!(
            out[..SPB * 3].iter().all(|x| x.abs() < 1e-3),
            "silent while the buffer is short"
        );
        assert!(
            out[SPB * 5..].iter().all(|x| (x - 0.5).abs() < 0.05),
            "plays once 4 beats are buffered"
        );
    }

    #[test]
    fn volume_map_gates_in_time() {
        let gate = vec![
            AutoPoint::at(0.0, 1.0),
            AutoPoint::at(2.0, 1.0),
            AutoPoint::at(2.0, 0.0),
            AutoPoint::at(4.0, 0.0),
        ];
        let mut w = make(flat(), gate, &[]);
        let out = run(&mut w, |_| 0.5, SPB * 4);
        assert!((out[SPB] - 0.5).abs() < 1e-2);
        assert!(out[SPB * 2 + 2000].abs() < 1e-3, "second half muted");
    }

    #[test]
    fn click_reduction_smooths_jumps() {
        // a sine with hold-jumps every 1/8: the largest sample step must stay near the sine's own
        let pts = factory_maps()
            .time
            .into_iter()
            .find(|s| s.name == "Stutter 1/8")
            .unwrap()
            .points;
        // 317 Hz: no whole number of periods in 1/8 beat, so each jump lands on another phase
        let sine = |i: usize| (i as f32 * 317.0 * std::f32::consts::TAU / SR).sin() * 0.5;
        let max_step = |click: f32| {
            let mut w = make(pts.clone(), flat(), &[(idx::CLICK, click), (idx::DC, 0.0)]);
            let out = run(&mut w, sine, SPB * 4);
            out[SPB / 2..]
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0f32, f32::max)
        };
        let smooth = max_step(1.0);
        let raw = max_step(0.0);
        let natural = 0.5 * 317.0 * std::f32::consts::TAU / SR;
        assert!(
            raw > natural * 4.0,
            "the jumps click without reduction ({raw})"
        );
        assert!(
            smooth < natural * 2.5,
            "click reduction leaves a step of {smooth} (sine's own {natural})"
        );
    }

    #[test]
    fn trigger_sync_waits_for_the_boundary() {
        let mut w = Warp::default();
        w.prepare(SR, 256);
        w.set_params(&params(&[(idx::TRIG, 3.0)])); // 1/4: next beat
        let half = vec![AutoPoint::at(0.0, 1.0), AutoPoint::at(4.0, 0.75)];
        let maps = WarpMaps {
            time: vec![
                WarpSlot {
                    name: "a".into(),
                    points: flat(),
                },
                WarpSlot {
                    name: "b".into(),
                    points: half,
                },
            ],
            vol: vec![],
        };
        w.swap_data(Box::new(maps));
        let _ = run(&mut w, |_| 0.0, 6000); // a quarter of a beat in
        w.set_params(&params(&[(idx::TRIG, 3.0), (idx::TSLOT, 1.0)]));
        assert_eq!(w.tb.active, 0);
        assert_eq!(w.tb.pending.map(|p| p.1), Some(1.0));
    }

    #[test]
    fn hold_last_bar_loops_it() {
        let mut w = make(flat(), flat(), &[]);
        let start = SPB * 8 + 100;
        let _ = run(&mut w, |_| 0.2, start);
        w.set_params(&params(&[(idx::HOLD, 1.0)]));
        let bps = 120.0 / 60.0 / SR as f64;
        let mut out = vec![];
        for b in 0..(SPB * 4 / 256) {
            let i = start + b * 256;
            let mut l: Vec<f32> = (i..i + 256).map(|_| 0.9).collect();
            let mut r = l.clone();
            w.process(
                &mut l,
                &mut r,
                &ProcessCtx {
                    sample_rate: SR,
                    bpm: 120.0,
                    beat: i as f64 * bps,
                    playing: true,
                },
            );
            out.extend_from_slice(&l);
        }
        // the new input (0.9) is never heard: the previous bar (0.2) plays again
        let bad: Vec<(usize, f32)> = out
            .iter()
            .copied()
            .enumerate()
            .skip(1000)
            .filter(|(_, x)| (x - 0.2).abs() >= 0.01)
            .take(5)
            .collect();
        assert!(
            bad.is_empty(),
            "hold must loop the old bar: {bad:?} hold {:?}",
            w.hold
        );
        assert!(w.tail_samples().is_none(), "a looping hold never parks");
    }

    #[test]
    fn stopped_transport_passes_through() {
        let mut w = make(
            vec![AutoPoint::at(0.0, 0.5), AutoPoint::at(4.0, 0.5)],
            flat(),
            &[],
        );
        let mut l = vec![0.3; 64];
        let mut r = l.clone();
        w.process(
            &mut l,
            &mut r,
            &ProcessCtx {
                sample_rate: SR,
                bpm: 120.0,
                beat: 0.0,
                playing: false,
            },
        );
        assert_eq!(l[LATENCY], 0.3);
    }

    #[test]
    fn factory_slots_are_valid() {
        let m = factory_maps();
        assert_eq!(m.time.len(), 16);
        assert_eq!(m.vol.len(), 16);
        for s in m.time.iter().chain(m.vol.iter()) {
            assert!(
                s.points.windows(2).all(|w| w[0].t <= w[1].t),
                "{} is sorted",
                s.name
            );
            assert!(
                s.points
                    .iter()
                    .all(|p| (0.0..=1.0).contains(&p.v) && (0.0..=WARP_BAR).contains(&p.t)),
                "{}",
                s.name
            );
        }
        let _ = CurveShape::Pulse;
    }
}
