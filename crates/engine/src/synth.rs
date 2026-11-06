//! The built-in pattern instruments (kick, clap, hat, bass) and the metronome click.
//! Same recipes as the prototype, rendered sample by sample from a fixed voice pool.

use std::f32::consts::TAU;

use crate::dsp::{Biquad, FilterType};
use crate::model::Inst;

pub const MAX_VOICES: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceKind {
    Kick,
    Clap,
    Hat,
    Bass,
    Click { accent: bool },
}

#[derive(Clone, Copy)]
pub struct Voice {
    pub kind: VoiceKind,
    /// Destination: track index, or `usize::MAX` for the click bus.
    pub dest: usize,
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
}

impl Voice {
    fn dead() -> Self {
        Self { kind: VoiceKind::Kick, dest: 0, delay: 0, t: 0, len: 0, freq: 0.0, phase: 0.0, filt: Biquad::default(), rng: 1, alive: false }
    }
}

pub struct VoicePool {
    pub voices: Vec<Voice>,
    sr: f32,
    seed: u32,
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

impl VoicePool {
    pub fn new(sr: f32) -> Self {
        Self { voices: vec![Voice::dead(); MAX_VOICES], sr, seed: 0x1234_5678 }
    }

    pub fn set_sr(&mut self, sr: f32) {
        self.sr = sr;
        self.clear();
    }

    pub fn clear(&mut self) {
        self.voices.iter_mut().for_each(|v| v.alive = false);
    }

    pub fn trigger(&mut self, kind: VoiceKind, dest: usize, delay: u32, len_s: f32, key: f32) {
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
            delay,
            t: 0,
            len: (len_s * self.sr) as u32,
            freq: mtof(key),
            phase: 0.0,
            filt,
            rng: self.seed | 1,
            alive: true,
        };
    }

    pub fn trigger_inst(&mut self, inst: Inst, dest: usize, delay: u32, len_s: f32, key: Option<u8>) {
        let kind = match inst {
            Inst::Kick => VoiceKind::Kick,
            Inst::Clap => VoiceKind::Clap,
            Inst::Hat => VoiceKind::Hat,
            Inst::Bass => VoiceKind::Bass,
        };
        self.trigger(kind, dest, delay, len_s, key.unwrap_or(36) as f32);
    }

    /// Adds every live voice into `tracks[dest]` (mono, both channels) or `click`.
    pub fn render(&mut self, n: usize, tracks: &mut [(Vec<f32>, Vec<f32>)], click: &mut [f32]) {
        let sr = self.sr;
        for v in self.voices.iter_mut().filter(|v| v.alive) {
            let start = (v.delay as usize).min(n);
            v.delay = v.delay.saturating_sub(n as u32);
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
                        let dt = v.freq / sr;
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
                };
                if v.dest == usize::MAX {
                    click[i] += s;
                } else if let Some((l, r)) = tracks.get_mut(v.dest) {
                    l[i] += s;
                    r[i] += s;
                }
                v.t += 1;
                if done {
                    v.alive = false;
                    break;
                }
            }
        }
    }
}
