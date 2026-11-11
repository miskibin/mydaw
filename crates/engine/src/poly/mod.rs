//! The built-in polyphonic synthesizer of Channel Rack channels of kind `synth`.
//!
//! One [`SynthInst`] per synth channel lives on the audio thread (in the [`SynthBank`]), made
//! off the audio thread by the engine and handed over through the command ring. Per instance:
//! 16 voices (plus 4 slots where stolen voices fade out), sample-accurate note events, two
//! global LFOs, then the global effects (drive, chorus, ping-pong delay, reverb), the
//! output trim and, for polyphonic patches, the chord headroom ([`POLY_HEADROOM`]). Voice signal flow:
//!
//! ```text
//! osc 1 ─┐ (wavetable, unison ≤ 7, detune, width)
//! osc 2 ─┤
//! sub ───┼─ drive (tanh, 2× oversampled) ─ SVF (LP12/LP24/BP/HP/notch) ─ amp env × vel ─ pan
//! noise ─┘
//! ```
//!
//! Real time: nothing here allocates, locks or does I/O once built; the wavetables are
//! generated once (see [`tables::bank`]) when the first instance is made.

pub mod fx;
pub mod lab;
pub mod params;
pub mod presets;
pub mod tables;
pub mod voice;

use std::f32::consts::TAU;

use serde::Serialize;

use crate::model::Id;
use crate::rt::{ChanRt, InsertRt};
use crate::synth::{pan_gains, Dest};
pub use params::{Patch, COUNT, PARAMS};
use params::{ix, SYNC_BEATS};
use voice::{EnvCoef, Voice};

/// Voices that can sound at once.
pub const POLY: usize = 16;
/// Extra slots where stolen voices fade out (so a steal never clicks).
const FADES: usize = 4;
/// Control-rate chunk: modulation of everything but the per-sample paths is updated this often.
pub const CHUNK: usize = 32;
const MAX_EVENTS: usize = 1024;
/// Synth channels the audio thread can hold.
pub const MAX_SYNTHS: usize = 64;
/// Steal fade time.
const STEAL_S: f32 = 0.004;
/// Fade when the transport restarts.
const STOP_S: f32 = 0.006;
/// Poly notes starting within this time of the first are one chord: with glide on they all
/// slide from the note before the chord, not from each other.
const CHORD_S: f32 = 0.03;
/// Output level of a polyphonic patch (−12 dB). Presets are levelled for one note; notes add
/// up, so a six-note chord of a pluck or keys patch is 8–13 dB louder than one note. This
/// headroom keeps such a chord near full scale while every note sums linearly: no limiter
/// squashing the chord (that made it pump and change tone as keys were added). Mono and legato
/// patches play one note at a time and keep their level.
pub const POLY_HEADROOM: f32 = 0.251;

fn headroom(mode: f32) -> f32 {
    if mode as u8 == 0 {
        POLY_HEADROOM
    } else {
        1.0
    }
}

#[derive(Clone, Copy, Debug)]
enum Ev {
    On { key: u8, vel: f32, id: u32, live: bool },
    Off { id: u32 },
}

#[derive(Clone, Copy, Debug)]
struct Event {
    at: u64,
    ev: Ev,
}

impl Event {
    /// Note-offs before note-ons at the same sample (touching notes do not slide).
    fn key(&self) -> (u64, u8) {
        (self.at, matches!(self.ev, Ev::On { .. }) as u8)
    }
}

#[derive(Clone, Copy, Default)]
struct Lfo {
    ph: f32,
    sh: f32,
    out: f32,
    rng: u32,
}

impl Lfo {
    fn fill(&mut self, wave: u8, hz: f32, sr: f32, out: &mut [f32]) {
        let dt = hz / sr;
        let slew = 1.0 - (-1.0 / (0.002 * sr)).exp();
        for o in out.iter_mut() {
            let ph = self.ph;
            let v = match wave {
                0 => (TAU * ph).sin(),
                1 => {
                    if ph < 0.25 {
                        4.0 * ph
                    } else if ph < 0.75 {
                        2.0 - 4.0 * ph
                    } else {
                        4.0 * ph - 4.0
                    }
                }
                2 => 1.0 - 2.0 * ph,
                3 => {
                    if ph < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                _ => self.sh,
            };
            // steps (square, S&H) are slewed over ~2 ms so they modulate without clicks
            self.out = if wave >= 3 { self.out + (v - self.out) * slew } else { v };
            *o = self.out;
            self.ph += dt;
            if self.ph >= 1.0 {
                self.ph -= 1.0;
                self.rng ^= self.rng << 13;
                self.rng ^= self.rng >> 17;
                self.rng ^= self.rng << 5;
                self.sh = (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0;
            }
        }
    }
}

/// One synth channel's sound engine.
pub struct SynthInst {
    pub chan: Id,
    sr: f32,
    target: Patch,
    cur: Patch,
    fresh: bool,
    voices: Vec<Voice>,
    events: Vec<Event>,
    now: u64,
    next_id: u32,
    /// Live key → note id (0 = up).
    live: [u32; 128],
    sustained: Vec<u32>,
    pedal: bool,
    /// Mono/legato: keys held, oldest first (key, note id, velocity, live).
    stack: [(u8, u32, f32, bool); 16],
    nstack: usize,
    last_st: Option<f32>,
    /// Poly glide: when the current group of notes struck together (a chord) began, and the
    /// key they all glide from (the note played before the chord).
    chord_at: Option<u64>,
    chord_from: Option<f32>,
    lfo: [Lfo; 2],
    lfo_buf: [[f32; CHUNK]; 2],
    wheel: f32,
    wheel_t: f32,
    bend: f32,
    /// Where the output goes: where the latest note was played.
    pub dest: Option<Dest>,
    fx: fx::Fx,
    bl: [f32; CHUNK],
    br: [f32; CHUNK],
    rng: u32,
    /// Matrix routing as it sounds: (source, destination, amount %). A changed source or
    /// destination first fades the old routing out, then the new one in.
    mslots: [(u8, usize, f32); 4],
    gain: (f32, f32),
    /// Output headroom as it sounds (glides when the voice mode changes, see [`POLY_HEADROOM`]).
    hr: f32,
    quiet: usize,
    idle: bool,
    age: u64,
}

impl SynthInst {
    /// A new instance for channel `chan` (allocates: call off the audio thread).
    pub fn new(chan: Id, patch: Patch, sr: f32) -> Self {
        tables::bank();
        Self {
            chan,
            sr,
            target: patch,
            cur: patch,
            fresh: false,
            voices: vec![Voice::default(); POLY + FADES],
            events: Vec::with_capacity(MAX_EVENTS),
            now: 0,
            next_id: 1,
            live: [0; 128],
            sustained: Vec::with_capacity(128),
            pedal: false,
            stack: [(0, 0, 0.0, false); 16],
            nstack: 0,
            last_st: None,
            chord_at: None,
            chord_from: None,
            lfo: [Lfo { rng: 0x2545_F491 ^ chan as u32 | 1, ..Default::default() }, Lfo { rng: 0x9E37_79B9 ^ chan as u32 | 1, ..Default::default() }],
            lfo_buf: [[0.0; CHUNK]; 2],
            wheel: 0.0,
            wheel_t: 0.0,
            bend: 0.0,
            dest: None,
            fx: fx::Fx::new(sr, CHUNK),
            bl: [0.0; CHUNK],
            br: [0.0; CHUNK],
            rng: 0x1234_5678 ^ (chan as u32).wrapping_mul(0x9E37_79B9) | 1,
            mslots: [(0, 0, 0.0); 4],
            gain: (0.0, 0.0),
            hr: headroom(patch[ix::V_MODE]),
            quiet: 0,
            idle: true,
            age: 0,
        }
    }

    pub fn patch(&self) -> &Patch {
        &self.target
    }

    /// New parameter values. Continuous ones glide over ~12 ms on sounding notes; discrete
    /// ones (waves, filter type, unison count) apply to notes started from now on.
    pub fn set_patch(&mut self, p: &Patch) {
        self.target = *p;
        if !self.voices.iter().any(|v| v.on) {
            self.fresh = false;
        }
    }

    /// One parameter (automation): glides like an edit when it is a continuous one.
    pub fn set_param(&mut self, idx: usize, v: f32) {
        if let Some(t) = self.target.get_mut(idx) {
            *t = v;
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.on).count()
    }

    fn new_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    fn push(&mut self, e: Event) -> bool {
        if self.events.len() >= self.events.capacity() {
            return false;
        }
        let k = e.key();
        let at = self.events.partition_point(|x| x.key() <= k);
        self.events.insert(at, e);
        true
    }

    /// A sequenced note: starts `delay` samples from now, lasts `len` samples.
    pub fn note(&mut self, delay: u32, len: u32, key: u8, vel: f32, dest: Dest) {
        if self.events.len() + 2 > self.events.capacity() {
            return;
        }
        let id = self.new_id();
        let at = self.now + delay as u64;
        self.dest = Some(dest);
        self.push(Event { at, ev: Ev::On { key, vel, id, live: false } });
        self.push(Event { at: at + len.max(1) as u64, ev: Ev::Off { id } });
    }

    /// A live key went down; it sounds until [`Self::key_up`] (and the pedal).
    pub fn key_down(&mut self, key: u8, vel: f32, dest: Dest) {
        let k = key as usize & 127;
        if self.events.len() + 2 > self.events.capacity() {
            return;
        }
        if self.live[k] != 0 {
            let old = self.live[k];
            self.push(Event { at: self.now, ev: Ev::Off { id: old } });
        }
        let id = self.new_id();
        self.live[k] = id;
        self.dest = Some(dest);
        self.push(Event { at: self.now, ev: Ev::On { key, vel, id, live: true } });
    }

    pub fn key_up(&mut self, key: u8) {
        let k = key as usize & 127;
        let id = std::mem::take(&mut self.live[k]);
        if id == 0 {
            return;
        }
        if self.pedal && self.sustained.len() < self.sustained.capacity() {
            self.sustained.push(id);
        } else {
            self.push(Event { at: self.now, ev: Ev::Off { id } });
        }
    }

    pub fn sustain(&mut self, down: bool) {
        self.pedal = down;
        if !down {
            while let Some(id) = self.sustained.pop() {
                self.push(Event { at: self.now, ev: Ev::Off { id } });
            }
        }
    }

    pub fn set_bend(&mut self, st: f32) {
        self.bend = st;
    }

    pub fn set_wheel(&mut self, v: f32) {
        self.wheel_t = v.clamp(0.0, 1.0);
    }

    /// Releases every live note and the pedal.
    pub fn all_notes_off(&mut self) {
        self.pedal = false;
        self.bend = 0.0;
        for k in 0..128 {
            let id = std::mem::take(&mut self.live[k]);
            if id != 0 {
                self.push(Event { at: self.now, ev: Ev::Off { id } });
            }
        }
        while let Some(id) = self.sustained.pop() {
            self.push(Event { at: self.now, ev: Ev::Off { id } });
        }
    }

    /// Transport restart: pending notes are dropped and everything sounding fades out fast.
    pub fn stop_all(&mut self) {
        self.events.clear();
        self.nstack = 0;
        self.live = [0; 128];
        self.sustained.clear();
        self.pedal = false;
        let step = 1.0 / (STOP_S * self.sr);
        for v in self.voices.iter_mut().filter(|v| v.on && v.fade_step == 0.0) {
            v.fade_step = step;
        }
    }

    fn glide_samples(&self) -> f32 {
        self.cur[ix::V_GLIDE] * 0.001 * self.sr
    }

    fn seed(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng
    }

    fn note_start(&mut self, key: u8, vel: f32, id: u32, live: bool) {
        if !self.fresh {
            self.cur = self.target;
            self.fresh = true;
        }
        self.idle = false;
        self.quiet = 0;
        self.age += 1;
        let glide = self.glide_samples();
        let mode = self.cur[ix::V_MODE] as u8;
        let seed = self.seed();
        let p = self.cur;
        if mode == 0 {
            if self.chord_at.is_none_or(|t| self.now.saturating_sub(t) > (CHORD_S * self.sr) as u64) {
                self.chord_at = Some(self.now);
                self.chord_from = self.last_st;
            }
            let from = if glide >= 1.0 { self.chord_from } else { None };
            let slot = self.free_slot();
            self.voices[slot].start(&p, id, key, vel, live, self.age, seed, from, glide);
        } else {
            // mono / legato: one voice, a stack of held keys (last note wins)
            if let Some(i) = self.stack[..self.nstack].iter().position(|s| s.1 == id) {
                self.stack.copy_within(i + 1..self.nstack, i);
                self.nstack -= 1;
            }
            if self.nstack == self.stack.len() {
                self.stack.copy_within(1.., 0);
                self.nstack -= 1;
            }
            self.stack[self.nstack] = (key, id, vel, live);
            self.nstack += 1;
            let v = &mut self.voices[0];
            let held = v.on && v.fade_step == 0.0 && !v.released();
            if held && mode == 2 {
                // legato: no retrigger, slide to the new key
                v.note = id;
                v.retarget(key, glide);
            } else {
                // a new note on the mono voice: the old one fades out in a spare slot while
                // the new one starts (from phase 0) and fades in, its envelopes continuing
                // from the old levels (retrigger from the current level)
                let (sounding, amp, menv, st) = (v.on && v.fade_step == 0.0, v.amp.v, v.menv.v, v.st);
                if v.on {
                    let f = self.fade_slot();
                    self.voices[f] = self.voices[0].clone();
                    if self.voices[f].fade_step == 0.0 {
                        self.voices[f].fade_step = 1.0 / (STEAL_S * self.sr);
                    }
                }
                let from = if glide >= 1.0 && mode == 1 { if sounding { Some(st) } else { self.last_st } } else { None };
                self.voices[0].start(&p, id, key, vel, live, self.age, seed, from, glide);
                if sounding && amp > 0.002 {
                    self.voices[0].take_over(amp, menv, STEAL_S * self.sr);
                }
            }
        }
        self.last_st = Some(key as f32);
    }

    fn note_stop(&mut self, id: u32) {
        let mode = self.cur[ix::V_MODE] as u8;
        if mode != 0 {
            if let Some(i) = self.stack[..self.nstack].iter().position(|s| s.1 == id) {
                self.stack.copy_within(i + 1..self.nstack, i);
                self.nstack -= 1;
            }
            let glide = self.glide_samples();
            let v = &mut self.voices[0];
            if v.on && v.note == id {
                if self.nstack > 0 {
                    // back to the last key still held, without retriggering
                    let (key, nid, _, _) = self.stack[self.nstack - 1];
                    v.note = nid;
                    v.retarget(key, glide);
                    self.last_st = Some(key as f32);
                } else {
                    v.release();
                }
                return;
            }
        }
        for v in self.voices.iter_mut().filter(|v| v.on && v.note == id) {
            v.release();
        }
    }

    /// A free voice slot, stealing (with a short fade) when all 16 sound: the oldest released
    /// voice first, else the oldest one.
    fn free_slot(&mut self) -> usize {
        if let Some(i) = self.voices[..POLY].iter().position(|v| !v.on) {
            return i;
        }
        let pick = (0..POLY).filter(|&i| self.voices[i].released()).min_by_key(|&i| self.voices[i].age).or_else(|| (0..POLY).min_by_key(|&i| self.voices[i].age)).unwrap_or(0);
        let f = self.fade_slot();
        self.voices[f] = self.voices[pick].clone();
        let step = 1.0 / (STEAL_S * self.sr);
        let fv = &mut self.voices[f];
        if fv.fade_step == 0.0 {
            fv.fade_step = step;
        }
        self.voices[pick].on = false;
        pick
    }

    fn fade_slot(&self) -> usize {
        (POLY..POLY + FADES).find(|&i| !self.voices[i].on).unwrap_or_else(|| (POLY..POLY + FADES).min_by(|&a, &b| self.voices[a].fade.total_cmp(&self.voices[b].fade)).unwrap_or(POLY))
    }

    fn apply_due(&mut self) {
        while let Some(e) = self.events.first().copied() {
            if e.at > self.now {
                break;
            }
            self.events.remove(0);
            match e.ev {
                Ev::On { key, vel, id, live } => self.note_start(key, vel, id, live),
                Ev::Off { id } => self.note_stop(id),
            }
        }
    }

    /// Per-chunk control: parameter smoothing, LFOs.
    fn control(&mut self, m: usize, bpm: f64) {
        let sr = self.sr;
        let k_amt = 1.0 - (-(m as f32) / (0.004 * sr)).exp();
        for s in 0..4 {
            let t = (self.target[ix::M1_SRC + 3 * s] as u8, self.target[ix::M1_DST + 3 * s] as usize, self.target[ix::M1_AMT + 3 * s]);
            let st = &mut self.mslots[s];
            if !self.fresh || st.0 == 0 || (st.0 == t.0 && st.1 == t.1) {
                if st.0 != t.0 || st.1 != t.1 {
                    *st = (t.0, t.1, if self.fresh { 0.0 } else { t.2 });
                }
                st.2 = if self.fresh { st.2 + (t.2 - st.2) * k_amt } else { t.2 };
            } else {
                st.2 -= st.2 * k_amt;
                if st.2.abs() < 0.05 {
                    *st = (t.0, t.1, 0.0);
                }
            }
        }
        if !self.fresh {
            self.cur = self.target;
            self.fresh = true;
        } else {
            let k = 1.0 - (-(m as f32) / (0.012 * sr)).exp();
            for i in 0..COUNT {
                let t = self.target[i];
                if !params::smoothed(i) {
                    self.cur[i] = t;
                } else if i == ix::FLT_CUTOFF {
                    let (a, b) = (self.cur[i].ln(), t.ln());
                    self.cur[i] = (a + (b - a) * k).exp();
                } else {
                    self.cur[i] += (t - self.cur[i]) * k;
                }
            }
        }
        self.wheel += (self.wheel_t - self.wheel) * (1.0 - (-(m as f32) / (0.01 * sr)).exp());
        for j in 0..2 {
            let base = if j == 0 { ix::L1_WAVE } else { ix::L2_WAVE };
            let sync = self.cur[base + 2] as usize;
            let hz = if sync > 0 { (bpm as f32 / 60.0) / SYNC_BEATS[sync.min(SYNC_BEATS.len() - 1)] } else { self.cur[base + 1] };
            let wave = self.cur[base] as u8;
            self.lfo[j].fill(wave, hz, sr, &mut self.lfo_buf[j][..m]);
        }
    }

    /// Renders `n` samples. Each finished stretch is handed to `sink(offset, l, r)` (already
    /// scaled by the output gains, which glide from the previous call's).
    pub fn process(&mut self, n: usize, bpm: f64, gl: f32, gr: f32, mut sink: impl FnMut(usize, &[f32], &[f32])) {
        let mut done = 0;
        while done < n {
            self.apply_due();
            let mut m = (n - done).min(CHUNK);
            if let Some(e) = self.events.first() {
                m = m.min((e.at - self.now) as usize).max(1);
            }
            if self.idle {
                self.gain = (gl, gr);
                self.hr = headroom(self.target[ix::V_MODE]);
                self.now += m as u64;
                done += m;
                continue;
            }
            self.control(m, bpm);
            self.bl[..m].fill(0.0);
            self.br[..m].fill(0.0);
            let (slots, nslots) = voice::slots(&self.mslots);
            let c = &self.cur;
            let ctx = voice::Ctx {
                bank: tables::bank(),
                sr: self.sr,
                p: c,
                lfo1: &self.lfo_buf[0][..m],
                lfo2: &self.lfo_buf[1][..m],
                wheel: self.wheel,
                bend: self.bend,
                amp: EnvCoef::new(c[ix::AMP_A], c[ix::AMP_D], c[ix::AMP_S] * 0.01, c[ix::AMP_R], self.sr),
                menv: EnvCoef::new(c[ix::MOD_A], c[ix::MOD_D], c[ix::MOD_S] * 0.01, c[ix::MOD_R], self.sr),
                slots,
                nslots,
            };
            let mut any = false;
            for v in self.voices.iter_mut().filter(|v| v.on) {
                v.render(&ctx, &mut self.bl[..m], &mut self.br[..m]);
                any |= v.on;
            }
            self.fx.process(&mut self.bl[..m], &mut self.br[..m], &self.cur, bpm);
            let trim = 10f32.powf(self.cur[ix::FX_OUT] / 20.0);
            let (g0l, g0r) = self.gain;
            let hr0 = self.hr;
            let hr1 = hr0 + (headroom(self.cur[ix::V_MODE]) - hr0) * (1.0 - (-(m as f32) / (0.012 * self.sr)).exp());
            let inv = 1.0 / m as f32;
            let mut pk = 0f32;
            for i in 0..m {
                let t = (i + 1) as f32 * inv;
                let h = trim * (hr0 + (hr1 - hr0) * t);
                let (a, b) = (self.bl[i] * h, self.br[i] * h);
                pk = pk.max(a.abs()).max(b.abs());
                self.bl[i] = a * (g0l + (gl - g0l) * t);
                self.br[i] = b * (g0r + (gr - g0r) * t);
            }
            self.gain = (gl, gr);
            self.hr = hr1;
            sink(done, &self.bl[..m], &self.br[..m]);
            if any || pk > 1e-6 {
                self.quiet = 0;
            } else {
                self.quiet += m;
                if self.quiet > self.fx.tail(&self.cur, bpm) + CHUNK {
                    self.idle = true;
                    self.fx.reset();
                }
            }
            self.now += m as u64;
            done += m;
        }
    }

    /// Standalone render (tests, tools): adds `l.len()` samples at unity gain, centre.
    pub fn render_into(&mut self, l: &mut [f32], r: &mut [f32], bpm: f64) {
        let n = l.len();
        self.process(n, bpm, 1.0, 1.0, |o, a, b| {
            for i in 0..a.len() {
                l[o + i] += a[i];
                r[o + i] += b[i];
            }
        });
    }
}

/// The audio thread's synth instances, by channel id.
pub struct SynthBank {
    insts: Vec<Box<SynthInst>>,
}

impl Default for SynthBank {
    fn default() -> Self {
        Self { insts: Vec::with_capacity(MAX_SYNTHS) }
    }
}

impl SynthBank {
    /// Takes an instance; returns what has to be thrown away (a replaced one, or this one
    /// when the bank is full).
    pub fn add(&mut self, inst: Box<SynthInst>) -> Option<Box<SynthInst>> {
        if let Some(i) = self.insts.iter().position(|x| x.chan == inst.chan) {
            return Some(std::mem::replace(&mut self.insts[i], inst));
        }
        if self.insts.len() < self.insts.capacity() {
            self.insts.push(inst);
            None
        } else {
            Some(inst)
        }
    }

    pub fn remove(&mut self, chan: Id) -> Option<Box<SynthInst>> {
        let i = self.insts.iter().position(|x| x.chan == chan)?;
        Some(self.insts.swap_remove(i))
    }

    pub fn get(&mut self, chan: Id) -> Option<&mut SynthInst> {
        self.insts.iter_mut().find(|x| x.chan == chan).map(|b| &mut **b)
    }

    pub fn len(&self) -> usize {
        self.insts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.insts.is_empty()
    }

    pub fn key_up(&mut self, chan: Id, key: u8) {
        if let Some(s) = self.get(chan) {
            s.key_up(key);
        }
    }

    pub fn sustain(&mut self, down: bool) {
        self.insts.iter_mut().for_each(|s| s.sustain(down));
    }

    pub fn set_bend(&mut self, st: f32) {
        self.insts.iter_mut().for_each(|s| s.set_bend(st));
    }

    pub fn set_wheel(&mut self, v: f32) {
        self.insts.iter_mut().for_each(|s| s.set_wheel(v));
    }

    pub fn all_notes_off(&mut self) {
        self.insts.iter_mut().for_each(|s| s.all_notes_off());
    }

    pub fn stop_all(&mut self) {
        self.insts.iter_mut().for_each(|s| s.stop_all());
    }

    /// Adds every instance's output into where its notes were played (a track buffer or an
    /// insert input), scaled by its channel's gain and pan.
    pub fn render(&mut self, n: usize, tracks: &mut [(Vec<f32>, Vec<f32>)], inserts: &mut [InsertRt], chans: &[ChanRt], bpm: f64) {
        for s in self.insts.iter_mut() {
            let (g, pan) = chans.iter().find(|c| c.id == s.chan).map(|c| (if c.on { c.gain } else { 0.0 }, c.pan)).unwrap_or((0.0, 0.0));
            let (pl, pr) = pan_gains(pan);
            let out: Option<(&mut [f32], &mut [f32])> = match s.dest {
                Some(Dest::Track(t)) => tracks.get_mut(t).map(|(l, r)| (&mut l[..], &mut r[..])),
                Some(Dest::Insert(k)) => inserts.get_mut(k).map(|i| (&mut i.l[..], &mut i.r[..])),
                _ => None,
            };
            match out {
                Some((l, r)) => s.process(n, bpm, g * pl, g * pr, |o, a, b| {
                    for i in 0..a.len() {
                        l[o + i] += a[i];
                        r[o + i] += b[i];
                    }
                }),
                None => s.process(n, bpm, 0.0, 0.0, |_, _, _| {}),
            }
        }
    }
}

// ---------------------------------------------------------------- catalog

#[derive(Clone, Debug, Serialize)]
pub struct SynthPreset {
    pub name: &'static str,
    pub category: &'static str,
    /// The complete parameter set (every catalog id).
    pub params: std::collections::BTreeMap<&'static str, f64>,
}

/// What the UI needs to draw the synth panel and its preset menu.
#[derive(Clone, Debug, Serialize)]
pub struct SynthCatalog {
    pub sections: Vec<params::SectionDef>,
    pub categories: &'static [&'static str],
    pub presets: Vec<SynthPreset>,
}

pub fn catalog() -> SynthCatalog {
    SynthCatalog {
        sections: params::sections(),
        categories: presets::CATEGORIES,
        presets: presets::all()
            .iter()
            .map(|p| SynthPreset { name: p.name, category: p.category, params: PARAMS.iter().zip(p.patch.iter()).map(|(d, v)| (d.id, (*v as f64 * 1000.0).round() / 1000.0)).collect() })
            .collect(),
    }
}

/// A synth state holding preset `name`'s full parameter set (`None` if there is no such preset).
pub fn preset_state(name: &str) -> Option<crate::model::SynthState> {
    let p = presets::all().into_iter().find(|p| p.name == name)?;
    Some(crate::model::SynthState { preset: Some(p.name.to_owned()), params: PARAMS.iter().zip(p.patch.iter()).map(|(d, v)| (d.id.to_owned(), (*v as f64 * 1000.0).round() / 1000.0)).collect() })
}
