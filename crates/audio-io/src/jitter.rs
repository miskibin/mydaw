//! Input jitter buffer: turns input packets that arrive on their own schedule (and, with a
//! separate input device, on their own clock) into a gap-free stream for the output
//! callback.
//!
//! Before: the output callback took whatever input had arrived and padded the rest of the
//! callback with zeros, and dropped frames once more than two callbacks piled up. With
//! 480-frame WASAPI input periods against 512-frame output callbacks the ring ran nearly
//! empty, so a packet arriving a moment late put ~477 zeros into the take several times a
//! second (heard as clicks), and the next pile-up dropped audio.
//!
//! Now:
//! - **Prefill**: nothing is consumed until the ring holds a target backlog (two input
//!   periods + half a callback + a jitter margin). The output never waits mid-stream for a
//!   packet that is merely late.
//! - **Drift steering**: the backlog is kept at the target by reading the input slightly
//!   faster or slower (±0.1 % at most) through a 32-tap windowed-sinc interpolator. At a
//!   step of exactly 1 the interpolator is an exact copy, so a device sharing the output's
//!   clock passes through untouched; a separate device's clock (±hundreds of ppm) is
//!   followed smoothly, never by dropping or repeating samples.
//! - **Real stalls** (the input stops delivering): the last sample fades to silence over
//!   2 ms, the buffer refills (with a larger margin) and the input fades back in. Overflow
//!   (the output stalled): fade out, skip to the target, fade in. Both are counted in the
//!   audio status (`inputUnderruns`, `inputOverruns`) and never produce a hard jump.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::OnceLock;

const TAPS: usize = 32;
/// Tap index of the frame the read position falls on (taps cover `-15 ..= +16`).
const CENTRE: usize = 15;
const PHASES: usize = 256;
const HIST: usize = 64;
/// Steering gain: backlog error (frames, smoothed) → step correction. 20 frames of error
/// ↔ 200 ppm; with the 0.5 s backlog smoothing the loop settles in ~2 s, critically damped.
const KP: f64 = 1e-5;
/// Callbacks after (re)priming before steering starts: the backlog it settled at becomes
/// the reference, so a device on the output's clock is never resampled.
const SETTLE: u32 = 50;
const MAX_CORR: f64 = 1e-3;

/// Counters for the audio status ("przerwy w wejściu"), written by the output callback.
#[derive(Debug, Default)]
pub(crate) struct IoStats {
    pub underruns: AtomicU32,
    pub overruns: AtomicU32,
    /// Measured clock difference input vs output in ppm (f32 bits): the steering correction.
    pub drift_ppm: AtomicU32,
}

impl IoStats {
    pub fn reset(&self) {
        self.underruns.store(0, Relaxed);
        self.overruns.store(0, Relaxed);
        self.drift_ppm.store(0f32.to_bits(), Relaxed);
    }
}

struct Sinc {
    h: Vec<[f32; TAPS]>,
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-12 * sum {
        term *= (x / (2.0 * k)) * (x / (2.0 * k));
        sum += term;
        k += 1.0;
    }
    sum
}

/// Kaiser-windowed sinc, PHASES + 1 rows (row `p` = fraction `p / PHASES`), each summing to 1.
fn sinc() -> &'static Sinc {
    static T: OnceLock<Sinc> = OnceLock::new();
    T.get_or_init(|| {
        let beta = 8.0;
        let half = TAPS as f64 / 2.0 + 0.5;
        let h = (0..=PHASES)
            .map(|p| {
                let f = p as f64 / PHASES as f64;
                let mut row = [0f64; TAPS];
                for (k, v) in row.iter_mut().enumerate() {
                    let t = k as f64 - CENTRE as f64 - f;
                    let s = if t.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t) };
                    let r = t / half;
                    let w = if r.abs() < 1.0 { bessel_i0(beta * (1.0 - r * r).sqrt()) / bessel_i0(beta) } else { 0.0 };
                    *v = s * w;
                }
                let sum: f64 = row.iter().sum();
                // whole-frame positions are exact copies (no rounding residue)
                if p == 0 || p == PHASES {
                    return std::array::from_fn(|k| if k == CENTRE + p / PHASES { 1.0 } else { 0.0 });
                }
                std::array::from_fn(|k| (row[k] / sum) as f32)
            })
            .collect();
        Sinc { h }
    })
}

/// Builds the interpolation table now (off the audio thread).
pub(crate) fn warm_up() {
    let _ = sinc();
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Waiting for the backlog to reach the target; outputs silence.
    Priming,
    Running,
    /// Fading to silence over `left` more frames: `hold` = no input left (underrun), the
    /// last sample decays; otherwise (overflow) the input keeps playing and then skips.
    FadeOut { left: u32, hold: bool },
}

pub(crate) struct JitterBuf {
    buf: [f32; HIST],
    /// Frames taken from the ring so far; frame `i` sits in `buf[i % HIST]`.
    have: u64,
    /// Read position (absolute input frame, fractional) of the next output frame.
    x: f64,
    mode: Mode,
    fade_in: u32,
    fade: u32,
    last: f32,
    corr: f64,
    fill_s: f64,
    /// Backlog the steering holds (NaN until settled after priming).
    anchor: f64,
    settle: u32,
    settle_sum: f64,
    /// Smoothed steering correction: the measured clock difference.
    drift: f64,
    /// No steering within this many frames of the anchor (jitter that the smoothing leaves).
    deadband: f64,
    /// Extra backlog for jitter, frames; grows after every underrun.
    margin: f64,
    max_margin: f64,
    /// Input periods kept in reserve: 2 for independent streams (WASAPI, CoreAudio…), 1 for
    /// ASIO, whose input buffer arrives in step with each output callback.
    periods: usize,
}

impl JitterBuf {
    pub fn new(rate: u32, periods: usize) -> Self {
        warm_up();
        let fade = (rate / 500).max(8);
        Self { buf: [0.0; HIST], have: 0, x: 0.0, mode: Mode::Priming, fade_in: 0, fade, last: 0.0, corr: 0.0, fill_s: 0.0, anchor: f64::NAN, settle: 0, settle_sum: 0.0, drift: 0.0, deadband: rate as f64 * 0.001, margin: rate as f64 * 0.002, max_margin: rate as f64 * 0.05, periods: periods.max(1) }
    }

    /// Backlog aimed for: `periods` input periods + half a callback + the interpolator's
    /// look-ahead + the jitter margin.
    fn target(&self, n: usize, packet: usize) -> f64 {
        (self.periods * packet + n / 2 + TAPS - CENTRE) as f64 + self.margin
    }

    /// Read position of the next output frame while real input flows (`None` while
    /// priming or fading out).
    pub fn pos(&self) -> Option<f64> {
        (self.mode == Mode::Running).then_some(self.x)
    }

    /// Frames buffered ahead of the read position.
    pub fn fill(&self, ring_frames: usize) -> f64 {
        if self.mode == Mode::Priming {
            ring_frames as f64
        } else {
            self.have as f64 - self.x + ring_frames as f64
        }
    }

    fn take(&mut self, ring: &mut rtrb::Consumer<f32>) -> bool {
        match (ring.pop(), ring.pop()) {
            (Ok(l), _) => {
                self.buf[(self.have % HIST as u64) as usize] = l;
                self.have += 1;
                true
            }
            _ => false,
        }
    }

    fn interp(&self) -> f32 {
        let i0 = self.x.floor();
        let f = (self.x - i0) * PHASES as f64;
        let p = (f.floor() as usize).min(PHASES - 1);
        let a = (f - p as f64) as f32;
        let t = sinc();
        let (h0, h1) = (&t.h[p], &t.h[p + 1]);
        let first = i0 as u64 - CENTRE as u64;
        let mut y = 0.0f32;
        for k in 0..TAPS {
            let s = self.buf[((first + k as u64) % HIST as u64) as usize];
            y += s * (h0[k] + a * (h1[k] - h0[k]));
        }
        y
    }

    /// Fills `out` (mono) for one output callback. `n_ref`: the callback size the backlog
    /// is planned for (max of this callback and the nominal buffer); `packet`: the input
    /// period in frames.
    pub fn pull(&mut self, ring: &mut rtrb::Consumer<f32>, out: &mut [f32], n_ref: usize, packet: usize, stats: &IoStats) {
        let n = out.len();
        let n_ref = n_ref.max(n);
        let packet_unknown = packet == 0;
        let packet = packet.max(1);
        let ring_frames = ring.slots() / 2;
        let target = self.target(n_ref, packet);
        if self.mode == Mode::Priming && packet_unknown {
            out.fill(0.0); // no packet seen yet: its size sets the target
            return;
        }
        match self.mode {
            Mode::Priming if ring_frames as f64 >= target => {
                // start reading at the first new frame; older history is faded in over
                self.x = self.have as f64 + CENTRE as f64;
                // (CENTRE frames of look-behind come from the ring too: start a little later)
                self.fill_s = self.fill(ring_frames);
                self.mode = Mode::Running;
                self.fade_in = self.fade;
                self.anchor = f64::NAN;
                self.settle = 0;
                self.settle_sum = 0.0;
            }
            Mode::Running => {
                let fill = self.fill(ring_frames);
                if fill > target + (4 * n_ref.max(packet)) as f64 + self.max_margin {
                    stats.overruns.fetch_add(1, Relaxed);
                    self.mode = Mode::FadeOut { left: self.fade, hold: false };
                } else {
                    self.fill_s = self.fill_s * 0.98 + fill * 0.02;
                    if self.anchor.is_nan() {
                        // the mean backlog over the first callbacks (not the slow average,
                        // which would still be on its way there)
                        self.settle += 1;
                        self.settle_sum += fill;
                        if self.settle >= SETTLE {
                            self.fill_s = self.settle_sum / SETTLE as f64;
                            // hold what it settled at, but never less than the target
                            self.anchor = self.fill_s.max(target);
                        }
                    } else {
                        // near the anchor nothing is steered: a shared clock stays an exact copy
                        let e = self.fill_s - self.anchor;
                        self.corr = (KP * e.signum() * (e.abs() - self.deadband).max(0.0)).clamp(-MAX_CORR, MAX_CORR);
                    }
                    self.drift = self.drift * 0.995 + self.corr * 0.005;
                    stats.drift_ppm.store(((self.drift * 1e6) as f32).to_bits(), Relaxed);
                }
            }
            _ => {}
        }
        for o in out.iter_mut() {
            match self.mode {
                Mode::Priming => *o = 0.0,
                Mode::FadeOut { left, hold: true } => {
                    *o = self.last * left as f32 / self.fade as f32;
                    self.step_fade(left, true, ring, target);
                }
                Mode::Running | Mode::FadeOut { hold: false, .. } => {
                    let need = self.x.floor() as u64 + (TAPS - CENTRE) as u64;
                    while self.have < need && self.take(ring) {}
                    if self.have < need {
                        // the input stopped delivering: fade out what was playing, refill
                        stats.underruns.fetch_add(1, Relaxed);
                        self.margin = (self.margin + n_ref as f64 / 4.0).min(self.max_margin);
                        self.mode = Mode::FadeOut { left: self.fade, hold: true };
                        *o = self.last;
                        continue;
                    }
                    let mut y = self.interp();
                    if self.mode == Mode::Running && self.fade_in > 0 {
                        y *= 1.0 - self.fade_in as f32 / self.fade as f32;
                        self.fade_in -= 1;
                    }
                    if let Mode::FadeOut { left, .. } = self.mode {
                        y *= left as f32 / self.fade as f32;
                        self.step_fade(left, false, ring, target);
                    }
                    self.last = y;
                    *o = y;
                    self.x += 1.0 + self.corr;
                }
            }
        }
    }

    fn step_fade(&mut self, left: u32, hold: bool, ring: &mut rtrb::Consumer<f32>, target: f64) {
        if left > 1 {
            self.mode = Mode::FadeOut { left: left - 1, hold };
            return;
        }
        self.last = 0.0;
        if hold {
            self.mode = Mode::Priming;
            return;
        }
        // overflow: skip ahead so the backlog is back at the target, then fade in
        let excess = (self.fill(ring.slots() / 2) - target).floor().max(0.0) as u64;
        for _ in 0..excess {
            if !self.take(ring) {
                break;
            }
        }
        self.x += excess as f64;
        self.mode = Mode::Running;
        self.fade_in = self.fade;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_clock_passes_the_input_through_untouched() {
        let (mut tx, mut rx) = rtrb::RingBuffer::<f32>::new(1 << 16);
        let src: Vec<f32> = (0..200_000).map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5).collect();
        let mut jb = JitterBuf::new(48000, 2);
        let stats = IoStats::default();
        let (mut out, mut o, mut fed) = (vec![], vec![0f32; 512], 0);
        for _ in 0..300 {
            // exactly one callback of input per callback, in 480-frame packets
            while fed < out.len() + 512 + 1500 {
                for &v in &src[fed..fed + 480] {
                    let _ = tx.push(v);
                    let _ = tx.push(v);
                }
                fed += 480;
            }
            jb.pull(&mut rx, &mut o, 512, 480, &stats);
            out.extend_from_slice(&o);
        }
        assert_eq!(stats.underruns.load(Relaxed), 0);
        let start = out.iter().position(|v| *v != 0.0).unwrap() + 200;
        let lag = (0..src.len() - 64).find(|&k| (0..64).all(|j| src[k + j] == out[start + j])).expect("the input passes through bit-exact");
        let bad = (start..out.len()).find(|&i| out[i] != src[lag + i - start]);
        assert_eq!(bad, None, "an interpolated sample (start {start}, len {}, fill {}, anchor {}, target {})", out.len(), jb.fill_s, jb.anchor, jb.target(512, 480));
    }
}
