//! The built-in pattern instruments (kick, clap, hat, bass), the one-shot sampler and the
//! metronome click. Same recipes as the prototype, rendered sample by sample from a fixed voice pool.
//!
//! Every note voice belongs to a channel: an index into the timeline's channel table
//! (re-mapped by channel id when a new timeline arrives). The voice reads its channel's
//! gain, pan and mute every block, so Channel Rack knobs also move notes that are ringing.

use std::f32::consts::TAU;

use crate::dsp::out_limiter::OutLimiter;
use crate::dsp::{Biquad, FilterType};
use crate::model::{Id, Inst};
use crate::rt::{ChanRt, InsertRt};

pub const MAX_VOICES: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceKind {
    Kick,
    Clap,
    Hat,
    Bass,
    /// One-shot sample of the voice's channel, pitched by the note.
    Sample,
    /// WAVE Keys: a recording of the channel's piano, picked by key and velocity; it rings
    /// for the note's length, then fades over the channel's release time.
    Keys,
    /// A synth channel: its notes go to the channel's `poly::SynthInst`, not this pool.
    Synth,
    Click { accent: bool },
}

impl VoiceKind {
    pub fn of(inst: Inst) -> Self {
        match inst {
            Inst::Kick => VoiceKind::Kick,
            Inst::Clap => VoiceKind::Clap,
            Inst::Hat => VoiceKind::Hat,
            Inst::Bass => VoiceKind::Bass,
        }
    }
}

/// Where a voice plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dest {
    /// A playlist track (then its fader and its insert).
    Track(usize),
    /// Straight into a mixer insert (by index).
    Insert(usize),
    /// The metronome bus.
    Click,
}

/// Voice without a channel (the metronome).
pub const NO_CHAN: usize = usize::MAX;

#[derive(Clone, Copy)]
pub struct Voice {
    pub kind: VoiceKind,
    pub dest: Dest,
    /// Index into the channel table, or [`NO_CHAN`].
    pub chan: usize,
    /// Id of that channel, to find it again in a rebuilt table.
    pub chan_id: Id,
    /// Note velocity (gain).
    vel: f32,
    /// Sampler: read position in source frames and step per output sample.
    pos: f64,
    rate: f64,
    /// Samples to wait before the voice starts (sub-block accuracy).
    pub delay: u32,
    t: u32,
    /// Note length in samples (bass).
    len: u32,
    freq: f32,
    phase: f32,
    filt: Biquad,
    rng: u32,
    pub alive: bool,
    /// Live (MIDI) note whose key is still down: it sounds until released.
    held: bool,
    /// Key let go while the sustain pedal was down: released when the pedal comes up.
    sus: bool,
    /// MIDI key of a live note (and of every piano note).
    key: u8,
    /// Piano: which recording of the bank plays, and the release envelope's level.
    zone: u16,
    env: f32,
}

impl Voice {
    fn dead() -> Self {
        Self {
            kind: VoiceKind::Kick,
            dest: Dest::Click,
            chan: NO_CHAN,
            chan_id: 0,
            vel: 1.0,
            pos: 0.0,
            rate: 1.0,
            delay: 0,
            t: 0,
            len: 0,
            freq: 0.0,
            phase: 0.0,
            filt: Biquad::default(),
            rng: 1,
            alive: false,
            held: false,
            sus: false,
            key: 0,
            zone: 0,
            env: 1.0,
        }
    }
}

/// WAVE Keys channels that can sound at once through their own output limiter (one bus per
/// channel and destination); a further one plays without it.
const KEYS_BUSES: usize = 8;
/// The longest block a Keys bus holds (`Engine::create_rt` clamps blocks to this).
const BUS_FRAMES: usize = 8192;

/// The summed voices of one WAVE Keys channel on their way to its destination: a piano chord
/// adds up far above full scale (ten notes: about +10 dBFS), so it passes the instruments'
/// output limiter first. Buffers are allocated with the pool, never on the audio thread.
struct KeysBus {
    chan_id: Id,
    dest: Dest,
    /// Fed by a voice this block.
    fed: bool,
    /// Samples the limiter still has to flush after the last voice ended (its look-ahead).
    tail: usize,
    used: bool,
    lim: OutLimiter,
    l: Vec<f32>,
    r: Vec<f32>,
}

pub struct VoicePool {
    pub voices: Vec<Voice>,
    keys: Vec<KeysBus>,
    sr: f32,
    seed: u32,
    /// Sustain pedal state (live MIDI).
    pedal: bool,
    /// Pitch-bend factor applied to live notes (1 = none).
    bend: f32,
}

#[inline]
fn exp_ramp(v0: f32, v1: f32, t: f32, dur: f32) -> f32 {
    v0 * (v1 / v0).powf((t / dur).min(1.0))
}

#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

pub fn mtof(key: f32) -> f32 {
    440.0 * 2f32.powf((key - 69.0) / 12.0)
}

/// Balance pan law shared with the mixer: the far side is attenuated linearly.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    (if pan > 0.0 { 1.0 - pan } else { 1.0 }, if pan < 0.0 { 1.0 + pan } else { 1.0 })
}

/// Ends a held voice with a 20 ms release (the bass tail; one-shots just ring out).
fn release(v: &mut Voice, sr: f32) {
    v.held = false;
    v.sus = false;
    // a piano note starts its own release (the channel's release time) right away
    v.len = if v.kind == VoiceKind::Keys { v.t } else { v.t.saturating_add((0.02 * sr) as u32) };
}

impl VoicePool {
    pub fn new(sr: f32) -> Self {
        let keys = (0..KEYS_BUSES)
            .map(|_| KeysBus { chan_id: 0, dest: Dest::Click, fed: false, tail: 0, used: false, lim: OutLimiter::new(sr), l: vec![0.0; BUS_FRAMES], r: vec![0.0; BUS_FRAMES] })
            .collect();
        Self { voices: vec![Voice::dead(); MAX_VOICES], keys, sr, seed: 0x1234_5678, pedal: false, bend: 1.0 }
    }

    pub fn set_sr(&mut self, sr: f32) {
        self.sr = sr;
        for b in &mut self.keys {
            b.lim = OutLimiter::new(sr);
        }
        self.clear();
    }

    pub fn clear(&mut self) {
        self.voices.iter_mut().for_each(|v| v.alive = false);
        for b in &mut self.keys {
            b.used = false;
            b.tail = 0;
            b.lim.reset();
        }
        self.pedal = false;
        self.bend = 1.0;
    }

    /// A live key went down: the note sounds until [`Self::note_off`] (and the pedal) let it go.
    /// Same routing and voices as a sequenced note; percussion and samples ring out by themselves.
    pub fn note_on(&mut self, ch: &ChanRt, idx: usize, dest: Dest, key: u8, vel: f32) -> bool {
        // the same key struck again releases its previous note
        self.release_where(|v| v.held && v.chan_id == ch.id && v.key == key);
        let Some(i) = self.note_slot(ch, idx, dest, 0, 3600.0, key, vel, 1.0) else { return false };
        let v = &mut self.voices[i];
        v.held = true;
        v.key = key;
        true
    }

    /// A live key went up. With the pedal down the note keeps sounding until it comes up.
    pub fn note_off(&mut self, chan_id: Id, key: u8) {
        let (pedal, sr) = (self.pedal, self.sr);
        for v in self.voices.iter_mut().filter(|v| v.alive && v.held && v.chan_id == chan_id && v.key == key) {
            if pedal {
                v.sus = true;
            } else {
                release(v, sr);
            }
        }
    }

    pub fn sustain(&mut self, down: bool) {
        self.pedal = down;
        if !down {
            self.release_where(|v| v.sus);
        }
    }

    /// Pitch bend of live notes, in semitones.
    pub fn set_bend(&mut self, semitones: f32) {
        self.bend = 2f32.powf(semitones / 12.0);
    }

    /// Lets go of every live note and the pedal (device unplugged, panic).
    pub fn all_notes_off(&mut self) {
        self.pedal = false;
        self.bend = 1.0;
        self.release_where(|v| v.held);
    }

    fn release_where(&mut self, f: impl Fn(&Voice) -> bool) {
        let sr = self.sr;
        for v in self.voices.iter_mut().filter(|v| v.alive && f(v)) {
            release(v, sr);
        }
    }

    /// A new channel table arrived: point every voice at its channel's new index, and
    /// silence voices whose channel is gone. RT-safe (no allocation).
    pub fn remap(&mut self, chans: &[ChanRt]) {
        for v in self.voices.iter_mut().filter(|v| v.alive && v.chan != NO_CHAN) {
            match chans.iter().position(|c| c.id == v.chan_id) {
                Some(i) => v.chan = i,
                None => v.alive = false,
            }
        }
    }

    /// The metronome click.
    pub fn click(&mut self, accent: bool, delay: u32) {
        self.trigger(VoiceKind::Click { accent }, Dest::Click, NO_CHAN, 0, delay, 0.06, 0.0, 1.0, 1.0);
    }

    /// A note of channel `ch` (index `idx` in the table). `key` is a MIDI key, `vel` the
    /// velocity 0..1 and `gain` level on top of it (the track gain when the channel skips the track).
    /// Returns false when nothing can play (a sampler channel without audio).
    #[allow(clippy::too_many_arguments)]
    pub fn note(&mut self, ch: &ChanRt, idx: usize, dest: Dest, delay: u32, len_s: f32, key: u8, vel: f32, gain: f32) -> bool {
        self.note_slot(ch, idx, dest, delay, len_s, key, vel, gain).is_some()
    }

    /// Like [`Self::note`], returning the voice slot that was started.
    #[allow(clippy::too_many_arguments)]
    fn note_slot(&mut self, ch: &ChanRt, idx: usize, dest: Dest, delay: u32, len_s: f32, key: u8, vel: f32, gain: f32) -> Option<usize> {
        let level = vel * gain;
        match ch.kind {
            VoiceKind::Sample => {
                let a = ch.sample.as_ref()?;
                let rate = a.sr as f64 / self.sr as f64 * 2f64.powf((key as f64 - ch.root as f64) / 12.0);
                Some(self.trigger(VoiceKind::Sample, dest, idx, ch.id, delay, len_s, key as f32, level, rate))
            }
            VoiceKind::Keys => {
                let bank = ch.keys.as_ref()?;
                let (zone, semis, hit) = bank.pick(key, vel, ch.keys_dynamics);
                let a = bank.audio(zone)?;
                let rate = a.sr as f64 / self.sr as f64 * 2f64.powf(semis as f64 / 12.0);
                // the same key struck again: the string's previous note is damped
                for v in self.voices.iter_mut().filter(|v| v.alive && v.kind == VoiceKind::Keys && v.chan_id == ch.id && v.key == key) {
                    v.held = false;
                    v.sus = false;
                    v.len = v.len.min(v.t);
                }
                let i = self.trigger(VoiceKind::Keys, dest, idx, ch.id, delay, len_s, key as f32, hit * gain, rate);
                self.voices[i].zone = zone;
                self.voices[i].key = key;
                Some(i)
            }
            VoiceKind::Synth => None,
            kind => Some(self.trigger(kind, dest, idx, ch.id, delay, len_s, key as f32, level, 1.0)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn trigger(&mut self, kind: VoiceKind, dest: Dest, chan: usize, chan_id: Id, delay: u32, len_s: f32, key: f32, vel: f32, rate: f64) -> usize {
        // steal the oldest voice if the pool is full
        let idx = match self.voices.iter().position(|v| !v.alive) {
            Some(i) => i,
            None => (0..self.voices.len()).max_by_key(|&i| self.voices[i].t).unwrap_or(0),
        };
        self.seed = self.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let mut filt = Biquad::default();
        match kind {
            VoiceKind::Hat => filt.set(FilterType::Highpass, self.sr, 7000.0, 0.707, 0.0),
            VoiceKind::Clap => filt.set(FilterType::Bandpass, self.sr, 1400.0, 0.9, 0.0),
            VoiceKind::Bass => filt.set(FilterType::Lowpass, self.sr, 900.0, 0.707, 0.0),
            _ => {}
        }
        self.voices[idx] = Voice {
            kind,
            dest,
            chan,
            chan_id,
            vel: vel.max(0.0),
            pos: 0.0,
            rate,
            delay,
            t: 0,
            len: (len_s * self.sr) as u32,
            freq: mtof(key),
            phase: 0.0,
            filt,
            rng: self.seed | 1,
            alive: true,
            held: false,
            sus: false,
            key: 0,
            zone: 0,
            env: 1.0,
        };
        idx
    }

    /// Adds every live voice into its destination: a track buffer, an insert's input, or
    /// the click bus. Built-in instruments are mono (then the channel's pan); samples are
    /// stereo (balance).
    pub fn render(&mut self, n: usize, tracks: &mut [(Vec<f32>, Vec<f32>)], inserts: &mut [InsertRt], click: &mut [f32], chans: &[ChanRt]) {
        let sr = self.sr;
        let bend = self.bend;
        let bussed = n <= BUS_FRAMES;
        for b in self.keys.iter_mut().filter(|b| b.used) {
            b.fed = false;
            b.l[..n.min(BUS_FRAMES)].fill(0.0);
            b.r[..n.min(BUS_FRAMES)].fill(0.0);
        }
        for v in self.voices.iter_mut().filter(|v| v.alive) {
            let bend = if v.held { bend } else { 1.0 };
            let start = (v.delay as usize).min(n);
            v.delay = v.delay.saturating_sub(n as u32);
            // channel gain × velocity, split by the channel's pan
            let (g, pan, g0, pan0, sample, piano) = match chans.get(v.chan) {
                Some(c) => {
                    let on = if c.on { v.vel } else { 0.0 };
                    (c.gain * on, c.pan, c.gain0 * on, c.pan0, c.sample.as_deref(), c.keys.as_deref().map(|b| (b, c.keys_release)))
                }
                None if v.chan == NO_CHAN => (v.vel, 0.0, v.vel, 0.0, None, None),
                None => {
                    v.alive = false;
                    continue;
                }
            };
            let (pl, pr) = pan_gains(pan);
            let (gl, gr) = (g * pl, g * pr);
            // an automated channel gain or pan glides from the previous block's value
            let (pl0, pr0) = pan_gains(pan0);
            let (gl0, gr0) = (g0 * pl0, g0 * pr0);
            let inv_n = 1.0 / n.max(1) as f32;
            let side = |i: usize| -> (f32, f32) {
                let t = i as f32 * inv_n;
                (gl0 + (gl - gl0) * t, gr0 + (gr - gr0) * t)
            };
            let (out_l, out_r): (&mut [f32], &mut [f32]) = match v.dest {
                // the click bus is mono
                Dest::Click => (&mut click[..], &mut []),
                Dest::Track(t) => match tracks.get_mut(t) {
                    Some((l, r)) => (&mut l[..], &mut r[..]),
                    None => {
                        v.alive = false;
                        continue;
                    }
                },
                Dest::Insert(k) => match inserts.get_mut(k) {
                    Some(ins) => (&mut ins.l[..], &mut ins.r[..]),
                    None => {
                        v.alive = false;
                        continue;
                    }
                },
            };
            let stereo = !out_r.is_empty();
            if v.kind == VoiceKind::Keys {
                let Some((a, release)) = piano.and_then(|(b, r)| b.audio(v.zone).map(|a| (a, r))) else {
                    v.alive = false;
                    continue;
                };
                // the channel's bus (through the limiter), or straight out when every bus is taken
                let bus = if bussed { keys_bus(&mut self.keys, v.chan_id, v.dest) } else { None };
                let (out_l, out_r, stereo): (&mut [f32], &mut [f32], bool) = match bus {
                    Some(b) => {
                        b.fed = true;
                        (&mut b.l[..n], &mut b.r[..n], true)
                    }
                    None => (out_l, out_r, stereo),
                };
                // -60 dB after `release` seconds
                let coef = 10f32.powf(-3.0 / (release.max(0.005) * sr));
                for i in start..n {
                    if v.pos >= a.frames as f64 {
                        v.alive = false;
                        break;
                    }
                    if v.t >= v.len {
                        v.env *= coef;
                        if v.env < 1e-4 {
                            v.alive = false;
                            break;
                        }
                    }
                    let (x, y) = a.sample(v.pos);
                    let fade = (v.t as f32 / (0.001 * sr)).min(1.0) * v.env;
                    let (gl, gr) = side(i);
                    out_l[i] += x * gl * fade;
                    if stereo {
                        out_r[i] += y * gr * fade;
                    }
                    v.pos += v.rate * bend as f64;
                    v.t += 1;
                }
                continue;
            }
            if v.kind == VoiceKind::Sample {
                let Some(a) = sample else {
                    v.alive = false;
                    continue;
                };
                for i in start..n {
                    if v.pos >= a.frames as f64 {
                        v.alive = false;
                        break;
                    }
                    let (x, y) = a.sample(v.pos);
                    // 2 ms fade-in against a click when the sample does not start at zero
                    let fade = (v.t as f32 / (0.002 * sr)).min(1.0);
                    let (gl, gr) = side(i);
                    out_l[i] += x * gl * fade;
                    if stereo {
                        out_r[i] += y * gr * fade;
                    }
                    v.pos += v.rate * bend as f64;
                    v.t += 1;
                }
                continue;
            }
            for i in start..n {
                let t = v.t as f32 / sr;
                let (s, done) = match v.kind {
                    VoiceKind::Kick => {
                        let f = exp_ramp(150.0, 42.0, t, 0.12);
                        v.phase = (v.phase + f / sr).fract();
                        ((v.phase * TAU).sin() * exp_ramp(0.9, 0.001, t, 0.38), t > 0.4)
                    }
                    VoiceKind::Hat | VoiceKind::Clap => {
                        v.rng ^= v.rng << 13;
                        v.rng ^= v.rng >> 17;
                        v.rng ^= v.rng << 5;
                        let noise = (v.rng as f32 / u32::MAX as f32) * 2.0 - 1.0;
                        let x = v.filt.tick(noise);
                        if v.kind == VoiceKind::Hat {
                            (x * exp_ramp(0.22, 0.001, t, 0.05), t > 0.06)
                        } else {
                            let a = if t < 0.003 { 0.7 * t / 0.003 } else { exp_ramp(0.7, 0.001, t - 0.003, 0.197) };
                            (x * a, t > 0.25)
                        }
                    }
                    VoiceKind::Bass => {
                        if v.t % 32 == 0 {
                            let fc = exp_ramp(900.0, 240.0, t, 0.2);
                            v.filt.set(FilterType::Lowpass, sr, fc, 0.707, 0.0);
                        }
                        let dt = v.freq * bend / sr;
                        v.phase += dt;
                        if v.phase >= 1.0 {
                            v.phase -= 1.0;
                        }
                        let saw = 2.0 * v.phase - 1.0 - poly_blep(v.phase, dt);
                        let d = v.len as f32 / sr;
                        let a = if t < 0.006 {
                            0.28 * t / 0.006
                        } else if t < d - 0.02 {
                            0.28
                        } else {
                            (0.28 * (d - t) / 0.02).max(0.0)
                        };
                        (v.filt.tick(saw) * a, t >= d)
                    }
                    VoiceKind::Click { accent } => {
                        let f = if accent { 1760.0 } else { 1320.0 };
                        v.phase = (v.phase + f / sr).fract();
                        let sq = if v.phase < 0.5 { 1.0 } else { -1.0 };
                        let a = if t < 0.002 { exp_ramp(0.0001, 0.22, t, 0.002) } else { exp_ramp(0.22, 0.0001, t - 0.002, 0.048) };
                        (sq * a, t > 0.06)
                    }
                    VoiceKind::Sample | VoiceKind::Synth | VoiceKind::Keys => (0.0, true),
                };
                let (gl, gr) = side(i);
                out_l[i] += s * gl;
                if stereo {
                    out_r[i] += s * gr;
                }
                v.t += 1;
                if done {
                    v.alive = false;
                    break;
                }
            }
        }
        self.flush_keys(n, tracks, inserts);
    }

    /// Each Keys bus through its limiter into its destination; a bus no voice feeds any more
    /// flushes the limiter's look-ahead, then is free again.
    fn flush_keys(&mut self, n: usize, tracks: &mut [(Vec<f32>, Vec<f32>)], inserts: &mut [InsertRt]) {
        for b in self.keys.iter_mut().filter(|b| b.used) {
            if b.fed {
                b.tail = OutLimiter::LOOK;
            }
            let (out_l, out_r): (&mut [f32], &mut [f32]) = match b.dest {
                Dest::Track(t) => match tracks.get_mut(t) {
                    Some((l, r)) => (&mut l[..], &mut r[..]),
                    None => (&mut [], &mut []),
                },
                Dest::Insert(k) => match inserts.get_mut(k) {
                    Some(ins) => (&mut ins.l[..], &mut ins.r[..]),
                    None => (&mut [], &mut []),
                },
                Dest::Click => (&mut [], &mut []),
            };
            let m = n.min(out_l.len()).min(b.l.len());
            let stereo = out_r.len() >= m;
            for i in 0..m {
                let (x, y) = b.lim.tick(b.l[i], b.r[i]);
                out_l[i] += x;
                if stereo {
                    out_r[i] += y;
                }
            }
            if !b.fed {
                b.tail = b.tail.saturating_sub(n);
                if b.tail == 0 {
                    b.used = false;
                    b.lim.reset();
                }
            }
        }
    }
}

/// The bus of `chan_id` playing into `dest`, or a free one claimed for it.
fn keys_bus(buses: &mut [KeysBus], chan_id: Id, dest: Dest) -> Option<&mut KeysBus> {
    let i = buses.iter().position(|b| b.used && b.chan_id == chan_id && b.dest == dest).or_else(|| buses.iter().position(|b| !b.used))?;
    let b = &mut buses[i];
    if !b.used {
        b.used = true;
        b.chan_id = chan_id;
        b.dest = dest;
        b.tail = 0;
        b.fed = false;
        b.l.fill(0.0);
        b.r.fill(0.0);
    }
    Some(b)
}
