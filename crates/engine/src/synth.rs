//! The sampler, the piano voices and the metronome click, rendered sample by sample from a
//! fixed voice pool.
//!
//! Every note voice belongs to a channel: an index into the timeline's channel table
//! (re-mapped by channel id when a new timeline arrives). The voice reads its channel's
//! gain, pan and mute every block, so Channel Rack knobs also move notes that are ringing.

use crate::model::Id;
use crate::rt::{ChanRt, InsertRt};

pub const MAX_VOICES: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceKind {
    /// The sample of the voice's channel, pitched by the note and shaped by the channel's
    /// sampler settings (part of the file, direction, envelope, one-shot or gate).
    Sample,
    /// WAVE Keys: a recording of the channel's piano, picked by key and velocity; it rings
    /// for the note's length, then fades over the channel's release time.
    Keys,
    /// A synth channel: its notes go to the channel's `poly::SynthInst`, not this pool.
    Synth,
    Click {
        accent: bool,
    },
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
    /// Note length in samples until release. A step trigger uses `u32::MAX`.
    len: u32,
    phase: f32,
    pub alive: bool,
    /// Live (MIDI) note whose key is still down: it sounds until released.
    held: bool,
    /// Key let go while the sustain pedal was down: released when the pedal comes up.
    sus: bool,
    /// MIDI key of a live note (and of every piano note).
    key: u8,
    /// Piano: which recording of the bank plays, and the release envelope's level.
    zone: u16,
    /// Release (and cut) level: 1 until the note ends, then it falls.
    env: f32,
    /// Sampler: the channel's cut group when the note started (0 = none), and the voice's
    /// own time at which a note of that group cuts it (`u32::MAX` = not cut).
    cut: u8,
    cut_at: u32,
}

impl Voice {
    fn dead() -> Self {
        Self {
            kind: VoiceKind::Sample,
            dest: Dest::Click,
            chan: NO_CHAN,
            chan_id: 0,
            vel: 1.0,
            pos: 0.0,
            rate: 1.0,
            delay: 0,
            t: 0,
            len: 0,
            phase: 0.0,
            alive: false,
            held: false,
            sus: false,
            key: 0,
            zone: 0,
            env: 1.0,
            cut: 0,
            cut_at: u32::MAX,
        }
    }
}

/// Output level of WAVE Keys against its recordings (−12 dB). The Salamander bank is
/// levelled hot: one full-velocity note peaks at +1 dBFS, ten keys at +10.5 dBFS. With this
/// headroom a loud note sits near −11 dBFS and a ten-note chord near −1.8 dBFS, so chords sum
/// linearly instead of being squashed by a limiter (which made them pump and change tone).
pub const KEYS_HEADROOM: f32 = 0.251;

pub struct VoicePool {
    pub voices: Vec<Voice>,
    sr: f32,
    /// Sustain pedal state (live MIDI).
    pedal: bool,
    /// Pitch-bend factor applied to live notes (1 = none).
    bend: f32,
}

#[inline]
fn exp_ramp(v0: f32, v1: f32, t: f32, dur: f32) -> f32 {
    v0 * (v1 / v0).powf((t / dur).min(1.0))
}

pub fn mtof(key: f32) -> f32 {
    440.0 * 2f32.powf((key - 69.0) / 12.0)
}

/// Balance pan law shared with the mixer: the far side is attenuated linearly.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    (if pan > 0.0 { 1.0 - pan } else { 1.0 }, if pan < 0.0 { 1.0 + pan } else { 1.0 })
}

/// Ends a held voice: its release (the channel's release time) starts now. A one-shot
/// sample ignores it and rings out.
fn release(v: &mut Voice) {
    v.held = false;
    v.sus = false;
    v.len = v.t;
}

/// Time for a cut voice to fall 60 dB: fast, but not a click.
const CUT_SECONDS: f32 = 0.006;
/// Fade at a trimmed edge of a sample, where the waveform does not start or end at zero.
const EDGE_SECONDS: f32 = 0.0015;

impl VoicePool {
    pub fn new(sr: f32) -> Self {
        Self { voices: vec![Voice::dead(); MAX_VOICES], sr, pedal: false, bend: 1.0 }
    }

    pub fn set_sr(&mut self, sr: f32) {
        self.sr = sr;
        self.clear();
    }

    pub fn clear(&mut self) {
        self.voices.iter_mut().for_each(|v| v.alive = false);
        self.pedal = false;
        self.bend = 1.0;
    }

    /// A live key went down: the note sounds until [`Self::note_off`] (and the pedal) let it go.
    /// Same routing and voices as a sequenced note; percussion and samples ring out by themselves.
    pub fn note_on(&mut self, ch: &ChanRt, idx: usize, dest: Dest, key: u8, vel: f32) -> bool {
        // the same key struck again releases its previous note
        self.release_where(|v| v.held && v.chan_id == ch.id && v.key == key);
        let Some(i) = self.note_slot(ch, idx, dest, 0, Some(3600.0), key, vel, 1.0) else { return false };
        let v = &mut self.voices[i];
        v.held = true;
        v.key = key;
        true
    }

    /// A live key went up. With the pedal down the note keeps sounding until it comes up.
    pub fn note_off(&mut self, chan_id: Id, key: u8) {
        let pedal = self.pedal;
        for v in self.voices.iter_mut().filter(|v| v.alive && v.held && v.chan_id == chan_id && v.key == key) {
            if pedal {
                v.sus = true;
            } else {
                release(v);
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
        for v in self.voices.iter_mut().filter(|v| v.alive && f(v)) {
            release(v);
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
        self.trigger(VoiceKind::Click { accent }, Dest::Click, NO_CHAN, 0, delay, Some(0.06), 1.0, 1.0);
    }

    /// A note of channel `ch` (index `idx` in the table). `key` is a MIDI key, `vel` the
    /// velocity 0..1 and `gain` level on top of it (the track gain when the channel skips the track).
    /// Returns false when nothing can play (a sampler channel without audio).
    #[allow(clippy::too_many_arguments)]
    pub fn note(&mut self, ch: &ChanRt, idx: usize, dest: Dest, delay: u32, len_s: Option<f32>, key: u8, vel: f32, gain: f32) -> bool {
        self.note_slot(ch, idx, dest, delay, len_s, key, vel, gain).is_some()
    }

    /// Like [`Self::note`], returning the voice slot that was started.
    #[allow(clippy::too_many_arguments)]
    fn note_slot(
        &mut self,
        ch: &ChanRt,
        idx: usize,
        dest: Dest,
        delay: u32,
        len_s: Option<f32>,
        key: u8,
        vel: f32,
        gain: f32,
    ) -> Option<usize> {
        let level = vel * gain;
        match ch.kind {
            VoiceKind::Sample => {
                let a = ch.sample.as_ref()?;
                let m = &ch.smp;
                let rate = a.sr as f64 / self.sr as f64 * 2f64.powf((key as f64 - ch.root as f64) / 12.0) * m.rate;
                // a note of a cut group silences what the group is playing, when it starts
                if m.cut != 0 {
                    for v in
                        self.voices.iter_mut().filter(|v| v.alive && v.kind == VoiceKind::Sample && v.cut == m.cut && v.cut_at == u32::MAX)
                    {
                        v.cut_at = v.t.saturating_add(delay.saturating_sub(v.delay));
                    }
                }
                let i = self.trigger(VoiceKind::Sample, dest, idx, ch.id, delay, len_s, level, rate);
                let (lo, hi) = region(m.start, m.end, a.frames);
                let v = &mut self.voices[i];
                v.pos = if m.reverse { hi - 1.0 } else { lo };
                v.cut = m.cut;
                v.key = key;
                Some(i)
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
                let i = self.trigger(VoiceKind::Keys, dest, idx, ch.id, delay, len_s, hit * gain * KEYS_HEADROOM, rate);
                self.voices[i].zone = zone;
                self.voices[i].key = key;
                Some(i)
            }
            VoiceKind::Synth | VoiceKind::Click { .. } => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn trigger(
        &mut self,
        kind: VoiceKind,
        dest: Dest,
        chan: usize,
        chan_id: Id,
        delay: u32,
        len_s: Option<f32>,
        vel: f32,
        rate: f64,
    ) -> usize {
        // steal the oldest voice if the pool is full
        let idx = match self.voices.iter().position(|v| !v.alive) {
            Some(i) => i,
            None => (0..self.voices.len()).max_by_key(|&i| self.voices[i].t).unwrap_or(0),
        };
        self.voices[idx] = Voice {
            kind,
            dest,
            chan,
            chan_id,
            vel: vel.max(0.0),
            rate,
            delay,
            len: len_s.map_or(u32::MAX, |seconds| (seconds * self.sr) as u32),
            alive: true,
            ..Voice::dead()
        };
        idx
    }

    /// Adds every live voice into its destination: a track buffer, an insert's input, or
    /// the click bus. Samples are stereo (the channel's pan is a balance).
    pub fn render(&mut self, n: usize, tracks: &mut [(Vec<f32>, Vec<f32>)], inserts: &mut [InsertRt], click: &mut [f32], chans: &[ChanRt]) {
        let sr = self.sr;
        let bend = self.bend;
        for v in self.voices.iter_mut().filter(|v| v.alive) {
            let bend = if v.held { bend } else { 1.0 };
            let start = (v.delay as usize).min(n);
            v.delay = v.delay.saturating_sub(n as u32);
            // channel gain × velocity, split by the channel's pan
            let (g, pan, g0, pan0, sample, piano) = match chans.get(v.chan) {
                Some(c) => {
                    let on = if c.on { v.vel } else { 0.0 } * if v.kind == VoiceKind::Sample { c.smp.gain } else { 1.0 };
                    (
                        c.gain * on,
                        c.pan,
                        c.gain0 * on,
                        c.pan0,
                        c.sample.as_deref().map(|a| (a, c.smp)),
                        c.keys.as_deref().map(|b| (b, c.keys_release)),
                    )
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
                let Some((a, m)) = sample else {
                    v.alive = false;
                    continue;
                };
                let (lo, hi) = region(m.start, m.end, a.frames);
                let (att, dec) = (m.attack * sr, m.decay * sr);
                let rel = fall(m.release, sr);
                let cut = fall(CUT_SECONDS, sr);
                // a trimmed or reversed part starts and ends inside the waveform: fade its edges
                let edge = EDGE_SECONDS * sr;
                let (edge_in, edge_out) = (m.reverse || m.start > 0.0, m.reverse || m.end < 1.0);
                let step = v.rate * bend as f64;
                let span = (hi - lo - 1.0).max(0.0);
                let fade_in = (m.fade_in * a.sr as f64).min(span);
                let fade_out = (m.fade_out * a.sr as f64).min(span);
                let in_scale = if fade_in > 0.0 { fade_in.recip() } else { 0.0 };
                let out_scale = if fade_out > 0.0 { fade_out.recip() } else { 0.0 };
                for i in start..n {
                    if v.pos < lo || v.pos >= hi {
                        v.alive = false;
                        break;
                    }
                    let t = v.t as f32;
                    let shape = if t < att {
                        t / att
                    } else if t < att + dec {
                        1.0 - (1.0 - m.sustain) * (t - att) / dec
                    } else {
                        m.sustain
                    };
                    if v.t >= v.cut_at {
                        v.env *= cut;
                    } else if m.gate && v.t >= v.len {
                        v.env *= rel;
                    }
                    // decayed to nothing, released or cut: the voice is over
                    if v.env < 1e-4 || (m.sustain <= 0.0 && t >= att + dec) {
                        v.alive = false;
                        break;
                    }
                    let mut fade = shape * v.env;
                    let from_start = if m.reverse { hi - 1.0 - v.pos } else { v.pos - lo };
                    let to_end = if m.reverse { v.pos - lo } else { hi - v.pos };
                    let edge_start = if edge_in { (t / edge).min(1.0) } else { 1.0 };
                    let edge_end = if edge_out { (to_end / step.max(1e-9) / edge as f64).min(1.0) as f32 } else { 1.0 };
                    let fade_start = if in_scale > 0.0 { (from_start * in_scale).clamp(0.0, 1.0) as f32 } else { 1.0 };
                    let fade_end = if out_scale > 0.0 {
                        let remaining = if m.reverse { to_end } else { to_end - 1.0 };
                        (remaining * out_scale).clamp(0.0, 1.0) as f32
                    } else {
                        1.0
                    };
                    fade *= edge_start.min(fade_start);
                    fade *= edge_end.min(fade_end);
                    let (x, y) = a.sample(v.pos);
                    let (gl, gr) = side(i);
                    out_l[i] += x * gl * fade;
                    if stereo {
                        out_r[i] += y * gr * fade;
                    }
                    v.pos += if m.reverse { -step } else { step };
                    v.t += 1;
                }
                continue;
            }
            // the metronome click
            let VoiceKind::Click { accent } = v.kind else {
                v.alive = false;
                continue;
            };
            for i in start..n {
                let t = v.t as f32 / sr;
                let f = if accent { 1760.0 } else { 1320.0 };
                v.phase = (v.phase + f / sr).fract();
                let sq = if v.phase < 0.5 { 1.0 } else { -1.0 };
                let a = if t < 0.002 { exp_ramp(0.0001, 0.22, t, 0.002) } else { exp_ramp(0.22, 0.0001, t - 0.002, 0.048) };
                let (gl, gr) = side(i);
                out_l[i] += sq * a * gl;
                if stereo {
                    out_r[i] += sq * a * gr;
                }
                v.t += 1;
                if t > 0.06 {
                    v.alive = false;
                    break;
                }
            }
        }
    }
}

/// Per-sample factor that falls 60 dB in `seconds`.
#[inline]
fn fall(seconds: f32, sr: f32) -> f32 {
    10f32.powf(-3.0 / (seconds.max(0.0005) * sr))
}

/// The frames `[lo, hi)` a sampler plays: fractions of the file, at least one frame.
#[inline]
fn region(start: f32, end: f32, frames: usize) -> (f64, f64) {
    let n = frames as f64;
    let lo = (start as f64 * n).floor().clamp(0.0, (n - 1.0).max(0.0));
    let hi = (end as f64 * n).ceil().clamp(lo + 1.0, n.max(lo + 1.0));
    (lo, hi)
}
