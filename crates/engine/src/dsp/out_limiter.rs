//! Output safety limiter shared by the instruments.

/// A transparent look-ahead peak limiter on an instrument's output (a synth instance, a WAVE
/// Keys channel). Instruments are levelled for one note; a chord adds up well above full
/// scale (eight voices of a keys patch: +9 dBFS, a ten-note piano chord: +10 dBFS) and
/// clipped hard further down the chain. The signal is delayed by [`OutLimiter::LOOK`]
/// samples (1.3 ms) so the gain is already down when a peak arrives: no waveshaping, no
/// clicks. Below the ceiling the gain is exactly 1.
#[derive(Clone, Copy)]
pub struct OutLimiter {
    env: f32,
    g: f32,
    rel: f32,
    att: f32,
    hold: u32,
    dl: [f32; Self::LOOK],
    dr: [f32; Self::LOOK],
    w: usize,
}

impl OutLimiter {
    const CEIL: f32 = 0.891; // −1 dBFS
    pub const LOOK: usize = 64;

    pub fn new(sr: f32) -> Self {
        let look = Self::LOOK as f32;
        Self {
            env: 0.0,
            g: 1.0,
            rel: (-1.0 / (0.150 * sr)).exp(),
            att: 1.0 - (-5.0 / look).exp(),
            hold: 0,
            dl: [0.0; Self::LOOK],
            dr: [0.0; Self::LOOK],
            w: 0,
        }
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.g = 1.0;
        self.hold = 0;
        self.dl = [0.0; Self::LOOK];
        self.dr = [0.0; Self::LOOK];
    }

    #[inline(always)]
    pub fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let pk = l.abs().max(r.abs());
        // a new peak is held for the look-ahead span, so the gain stays down until it has
        // passed through the delay, then releases
        if pk >= self.env {
            self.env = pk;
            self.hold = Self::LOOK as u32;
        } else if self.hold > 0 {
            self.hold -= 1;
        } else {
            self.env = pk.max(self.env * self.rel);
        }
        let target = if self.env > Self::CEIL {
            Self::CEIL / self.env
        } else {
            1.0
        };
        if target < self.g {
            self.g += (target - self.g) * self.att;
        } else {
            self.g = target
                .min(self.g + (1.0 - self.g) * (1.0 - self.rel) + 1e-7)
                .min(1.0);
        }
        let (ol, or) = (self.dl[self.w], self.dr[self.w]);
        self.dl[self.w] = l;
        self.dr[self.w] = r;
        self.w = (self.w + 1) % Self::LOOK;
        (
            (ol * self.g).clamp(-1.0, 1.0),
            (or * self.g).clamp(-1.0, 1.0),
        )
    }
}
