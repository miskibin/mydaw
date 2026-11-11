//! Factory presets. Each one is a patch over the defaults; `fx_out` (the output trim) of every
//! preset was set by measurement so a fixed probe lands in the same loudness band (see
//! docs/synth.md and the `synth_lab levels` example).

use super::params::{defaults, ix, Patch};

pub const CATEGORIES: &[&str] = &["808", "Bas", "Pluck", "Lead", "Pad", "Klawisze", "Dzwonki", "Dęte/Smyczki", "Arp", "Lo-fi", "FX"];

pub struct Preset {
    pub name: &'static str,
    pub category: &'static str,
    pub patch: Patch,
}

impl Preset {
    /// Basses are probed at C3, everything else at C4.
    pub fn probe_key(&self) -> u8 {
        if matches!(self.category, "808" | "Bas") {
            48
        } else {
            60
        }
    }
}

// choice values
const SINE: f32 = 0.0;
const TRI: f32 = 1.0;
const SAW: f32 = 2.0;
const SQR: f32 = 3.0;
const HOLLOW: f32 = 4.0;
const AIR: f32 = 5.0;
const DIGITAL: f32 = 6.0;
const METAL: f32 = 7.0;
const LP24: f32 = 1.0;
const BP: f32 = 2.0;
const MONO: f32 = 1.0;
const LEGATO: f32 = 2.0;
// matrix sources
const MENV: f32 = 1.0;
const LFO1: f32 = 2.0;
const LFO2: f32 = 3.0;
const VEL: f32 = 4.0;
const RND: f32 = 6.0;
const VIB: f32 = 8.0;
// matrix destinations
const PITCH: f32 = 0.0;
const SH1: f32 = 3.0;
const L2: f32 = 6.0;
const CUT: f32 = 7.0;
const PAN: f32 = 9.0;
const DETUNE: f32 = 10.0;
const AMP: f32 = 12.0;
const NOISE: f32 = 13.0;
/// Pitch destinations: ±100 % = ±48 st.
const fn st(semis: f32) -> f32 {
    semis / 48.0 * 100.0
}
/// Cutoff destination: ±100 % = ±8 octaves.
const fn oct(o: f32) -> f32 {
    o / 8.0 * 100.0
}

fn pr(name: &'static str, category: &'static str, kv: &[(usize, f32)]) -> Preset {
    let mut patch = defaults();
    for &(i, v) in kv {
        patch[i] = v;
    }
    Preset { name, category, patch }
}

/// Matrix slot `s` (0..4): source, destination, amount %.
fn m(s: usize, src: f32, dst: f32, amt: f32) -> [(usize, f32); 3] {
    [(ix::M1_SRC + 3 * s, src), (ix::M1_DST + 3 * s, dst), (ix::M1_AMT + 3 * s, amt)]
}

fn with(base: &[(usize, f32)], slots: &[[(usize, f32); 3]]) -> Vec<(usize, f32)> {
    let mut v = base.to_vec();
    for s in slots {
        v.extend_from_slice(s);
    }
    v
}

pub fn all() -> Vec<Preset> {
    use ix::*;
    vec![
        // ------------------------------------------------ 808 / sub
        pr("808 Czysty", "808", &with(
            &[(O1_WAVE, SINE), (O1_LEVEL, 90.0), (FLT_CUTOFF, 12000.0), (FLT_RES, 0.0), (AMP_A, 2.0), (AMP_D, 1.0), (AMP_S, 100.0), (AMP_R, 160.0), (MOD_A, 0.5), (MOD_D, 75.0), (MOD_S, 0.0), (MOD_R, 60.0), (V_MODE, LEGATO), (V_GLIDE, 110.0), (FX_DRIVE, 18.0), (FX_OUT, -12.8)],
            &[m(0, MENV, PITCH, st(24.0))],
        )),
        pr("808 Brudny", "808", &with(
            &[(O1_WAVE, SINE), (O1_SHAPE, 45.0), (O1_LEVEL, 90.0), (O2_WAVE, TRI), (O2_LEVEL, 24.0), (FLT_CUTOFF, 6500.0), (FLT_RES, 25.0), (FLT_DRIVE, 62.0), (AMP_A, 1.5), (AMP_D, 1.0), (AMP_S, 100.0), (AMP_R, 180.0), (MOD_A, 0.5), (MOD_D, 55.0), (MOD_S, 0.0), (V_MODE, LEGATO), (V_GLIDE, 135.0), (FX_DRIVE, 40.0), (FX_OUT, -11.6)],
            &[m(0, MENV, PITCH, st(31.0))],
        )),
        pr("808 Twardy", "808", &with(
            &[(O1_WAVE, SINE), (O1_SHAPE, 100.0), (O1_LEVEL, 90.0), (SUB_LEVEL, 0.0), (FLT_CUTOFF, 4000.0), (FLT_RES, 15.0), (FLT_DRIVE, 80.0), (AMP_A, 1.0), (AMP_S, 100.0), (AMP_R, 140.0), (MOD_A, 0.5), (MOD_D, 35.0), (MOD_S, 0.0), (V_MODE, LEGATO), (V_GLIDE, 90.0), (FX_DRIVE, 45.0), (FX_OUT, -10.6)],
            &[m(0, MENV, PITCH, st(24.0))],
        )),
        pr("808 Phonk", "808", &with(
            &[(O1_WAVE, SINE), (O1_SHAPE, 70.0), (O1_LEVEL, 90.0), (O2_WAVE, TRI), (O2_LEVEL, 28.0), (O2_COARSE, 12.0), (FLT_CUTOFF, 3800.0), (FLT_RES, 20.0), (FLT_DRIVE, 70.0), (AMP_A, 1.5), (AMP_D, 1.0), (AMP_S, 100.0), (AMP_R, 260.0), (MOD_A, 0.5), (MOD_D, 45.0), (MOD_S, 0.0), (V_MODE, LEGATO), (V_GLIDE, 170.0), (FX_DRIVE, 58.0), (FX_OUT, -9.6)],
            &[m(0, MENV, PITCH, st(36.0))],
        )),
        pr("Sub Bas", "808", &with(
            &[(O1_WAVE, SINE), (O1_LEVEL, 90.0), (FLT_CUTOFF, 900.0), (FLT_RES, 0.0), (AMP_A, 3.0), (AMP_S, 100.0), (AMP_R, 120.0), (MOD_A, 0.5), (MOD_D, 30.0), (MOD_S, 0.0), (V_MODE, LEGATO), (V_GLIDE, 60.0), (FX_OUT, -12.9)],
            &[m(0, MENV, PITCH, st(7.0))],
        )),
        // ------------------------------------------------ bass
        pr("Reese Szeroki", "Bas", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 70.0), (O2_WAVE, SAW), (O2_LEVEL, 70.0), (O2_FINE, -17.0), (UNI_VOICES, 2.0), (UNI_DETUNE, 38.0), (UNI_WIDTH, 70.0), (SUB_LEVEL, 35.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 950.0), (FLT_RES, 38.0), (FLT_DRIVE, 55.0), (AMP_A, 5.0), (AMP_S, 100.0), (AMP_R, 150.0), (L1_WAVE, 0.0), (L1_RATE, 0.25), (V_MODE, LEGATO), (V_GLIDE, 40.0), (FX_DRIVE, 30.0), (FX_CHORUS, 15.0), (FX_OUT, -11.3)],
            &[m(0, LFO1, CUT, oct(0.8))],
        )),
        pr("Bas Pluck", "Bas", &[(O1_WAVE, SAW), (O1_LEVEL, 75.0), (O2_WAVE, SQR), (O2_LEVEL, 45.0), (O2_COARSE, -12.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 380.0), (FLT_RES, 30.0), (FLT_ENV, 3.0), (MOD_A, 8.0), (MOD_D, 600.0), (MOD_S, 0.0), (AMP_A, 1.0), (AMP_D, 900.0), (AMP_S, 60.0), (AMP_R, 80.0), (V_MODE, MONO), (FX_OUT, -6.9)]),
        pr("Bas Analog", "Bas", &[(O1_WAVE, SQR), (O1_SHAPE, 20.0), (O1_LEVEL, 70.0), (O2_WAVE, SAW), (O2_LEVEL, 55.0), (O2_FINE, 6.0), (SUB_LEVEL, 30.0), (FLT_CUTOFF, 1200.0), (FLT_RES, 20.0), (FLT_KEY, 50.0), (FLT_ENV, 2.0), (MOD_A, 0.5), (MOD_D, 250.0), (MOD_S, 10.0), (AMP_A, 2.0), (AMP_S, 100.0), (AMP_R, 90.0), (V_MODE, LEGATO), (V_GLIDE, 30.0), (FX_OUT, -13.4)]),
        pr("Wobble", "Bas", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 75.0), (O2_WAVE, SQR), (O2_LEVEL, 55.0), (O2_COARSE, -12.0), (UNI_VOICES, 1.0), (UNI_DETUNE, 20.0), (UNI_WIDTH, 30.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 300.0), (FLT_RES, 62.0), (FLT_DRIVE, 65.0), (AMP_A, 2.0), (AMP_S, 100.0), (AMP_R, 100.0), (L1_WAVE, 0.0), (L1_SYNC, 6.0), (V_MODE, LEGATO), (V_GLIDE, 25.0), (FX_DRIVE, 40.0), (FX_OUT, -10.1)],
            &[m(0, LFO1, CUT, oct(2.6))],
        )),
        pr("Reese Growl", "Bas", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 70.0), (O2_WAVE, SAW), (O2_LEVEL, 70.0), (O2_FINE, -23.0), (UNI_VOICES, 2.0), (UNI_DETUNE, 42.0), (UNI_WIDTH, 80.0), (SUB_LEVEL, 30.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 650.0), (FLT_RES, 45.0), (FLT_DRIVE, 70.0), (AMP_A, 4.0), (AMP_S, 100.0), (AMP_R, 120.0), (L1_WAVE, 1.0), (L1_SYNC, 10.0), (L2_WAVE, 0.0), (L2_RATE, 0.4), (V_MODE, LEGATO), (V_GLIDE, 50.0), (FX_DRIVE, 45.0), (FX_OUT, -10.1)],
            &[m(0, LFO1, CUT, oct(2.2)), m(1, LFO2, DETUNE, 35.0)],
        )),
        pr("Hoover", "Bas", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 70.0), (O2_WAVE, SQR), (O2_SHAPE, 40.0), (O2_LEVEL, 55.0), (O2_COARSE, -12.0), (UNI_VOICES, 3.0), (UNI_DETUNE, 72.0), (UNI_WIDTH, 90.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 3500.0), (FLT_RES, 25.0), (FLT_DRIVE, 40.0), (MOD_A, 0.5), (MOD_D, 320.0), (MOD_S, 0.0), (AMP_A, 3.0), (AMP_S, 100.0), (AMP_R, 160.0), (L1_RATE, 5.0), (V_MODE, LEGATO), (V_GLIDE, 90.0), (FX_DRIVE, 35.0), (FX_CHORUS, 40.0), (FX_REVERB, 12.0), (FX_OUT, -10.4)],
            &[m(0, MENV, PITCH, st(5.0)), m(1, VIB, PITCH, st(0.4))],
        )),
        pr("Acid", "Bas", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 85.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 300.0), (FLT_RES, 78.0), (FLT_DRIVE, 55.0), (FLT_KEY, 30.0), (FLT_ENV, 3.8), (MOD_A, 0.5), (MOD_D, 190.0), (MOD_S, 0.0), (AMP_A, 1.5), (AMP_D, 400.0), (AMP_S, 100.0), (AMP_R, 70.0), (V_MODE, MONO), (V_GLIDE, 70.0), (FX_DRIVE, 30.0), (FX_DELAY, 14.0), (FX_DTIME, 3.0), (FX_OUT, -13.6)],
            &[m(0, VEL, CUT, oct(1.0))],
        )),
        // ------------------------------------------------ pluck
        pr("Pluck Trap", "Pluck", &[(FX_DRIVE, 28.0), (O1_WAVE, SAW), (O1_COARSE, 12.0), (O1_LEVEL, 60.0), (O2_WAVE, SQR), (O2_LEVEL, 50.0), (UNI_VOICES, 1.0), (UNI_DETUNE, 20.0), (UNI_WIDTH, 70.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 700.0), (FLT_RES, 28.0), (FLT_DRIVE, 35.0), (FLT_ENV, 5.0), (MOD_A, 0.5), (MOD_D, 260.0), (MOD_S, 0.0), (AMP_A, 1.0), (AMP_D, 800.0), (AMP_S, 0.0), (AMP_R, 150.0), (FX_DELAY, 18.0), (FX_DTIME, 3.0), (FX_DFB, 30.0), (FX_REVERB, 15.0), (FX_RSIZE, 1.6), (FX_OUT, 0.6)]),
        pr("Pluck Szklany", "Pluck", &with(
            &[(FX_DRIVE, 12.0), (O1_WAVE, AIR), (O1_LEVEL, 70.0), (O2_WAVE, DIGITAL), (O2_LEVEL, 35.0), (O2_COARSE, 12.0), (FLT_CUTOFF, 2000.0), (FLT_RES, 15.0), (FLT_ENV, 3.0), (MOD_A, 0.5), (MOD_D, 300.0), (MOD_S, 0.0), (AMP_A, 1.0), (AMP_D, 1600.0), (AMP_S, 0.0), (AMP_R, 400.0), (FX_CHORUS, 30.0), (FX_REVERB, 25.0), (FX_RSIZE, 2.2), (FX_OUT, 1.8)],
            &[m(0, VEL, CUT, oct(1.0))],
        )),
        pr("Pluck Ciepły", "Pluck", &[(FX_DRIVE, 15.0), (O1_WAVE, TRI), (O1_LEVEL, 70.0), (O2_WAVE, SAW), (O2_LEVEL, 45.0), (UNI_VOICES, 1.0), (UNI_DETUNE, 15.0), (UNI_WIDTH, 60.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 1500.0), (FLT_RES, 10.0), (FLT_ENV, 2.5), (MOD_A, 0.5), (MOD_D, 250.0), (MOD_S, 0.0), (AMP_A, 1.5), (AMP_D, 1400.0), (AMP_S, 0.0), (AMP_R, 300.0), (FX_REVERB, 20.0), (FX_OUT, 0.5)]),
        pr("Pluck Hard", "Pluck", &[(O1_WAVE, SAW), (O1_LEVEL, 70.0), (O2_WAVE, SQR), (O2_LEVEL, 55.0), (O2_COARSE, 12.0), (UNI_VOICES, 1.0), (UNI_DETUNE, 18.0), (UNI_WIDTH, 60.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 400.0), (FLT_RES, 42.0), (FLT_DRIVE, 60.0), (FLT_ENV, 5.5), (MOD_A, 0.5), (MOD_D, 140.0), (MOD_S, 0.0), (AMP_A, 0.8), (AMP_D, 650.0), (AMP_S, 0.0), (AMP_R, 90.0), (FX_DRIVE, 35.0), (FX_DELAY, 15.0), (FX_DTIME, 3.0), (FX_REVERB, 12.0), (FX_OUT, 0.8)]),
        pr("Pluck EDM", "Pluck", &[(O1_WAVE, SAW), (O1_LEVEL, 70.0), (O2_WAVE, SQR), (O2_LEVEL, 40.0), (O2_COARSE, 12.0), (UNI_VOICES, 2.0), (UNI_DETUNE, 40.0), (UNI_WIDTH, 95.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 800.0), (FLT_RES, 20.0), (FLT_ENV, 4.2), (MOD_A, 0.5), (MOD_D, 230.0), (MOD_S, 0.0), (AMP_A, 1.0), (AMP_D, 900.0), (AMP_S, 0.0), (AMP_R, 200.0), (FX_DRIVE, 15.0), (FX_CHORUS, 20.0), (FX_DELAY, 22.0), (FX_DTIME, 3.0), (FX_DFB, 38.0), (FX_REVERB, 20.0), (FX_RSIZE, 1.8), (FX_OUT, -0.2)]),
        // ------------------------------------------------ lead
        pr("Supersaw Szeroki", "Lead", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 75.0), (O2_WAVE, SAW), (O2_LEVEL, 45.0), (O2_COARSE, 12.0), (O2_FINE, 9.0), (UNI_VOICES, 3.0), (UNI_DETUNE, 72.0), (UNI_WIDTH, 100.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 9500.0), (FLT_RES, 6.0), (AMP_A, 8.0), (AMP_D, 600.0), (AMP_S, 90.0), (AMP_R, 300.0), (L1_RATE, 5.2), (FX_DRIVE, 12.0), (FX_CHORUS, 28.0), (FX_DELAY, 14.0), (FX_DTIME, 3.0), (FX_REVERB, 26.0), (FX_RSIZE, 2.2), (FX_OUT, -8.3)],
            &[m(0, VIB, PITCH, st(0.4))],
        )),
        pr("Supersaw Lead", "Lead", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 75.0), (O2_WAVE, SAW), (O2_LEVEL, 35.0), (O2_COARSE, 12.0), (UNI_VOICES, 3.0), (UNI_DETUNE, 58.0), (UNI_WIDTH, 100.0), (FLT_CUTOFF, 8500.0), (FLT_RES, 10.0), (AMP_A, 5.0), (AMP_D, 500.0), (AMP_S, 85.0), (AMP_R, 250.0), (L1_WAVE, 0.0), (L1_RATE, 5.5), (V_GLIDE, 55.0), (FX_CHORUS, 25.0), (FX_DELAY, 15.0), (FX_DTIME, 3.0), (FX_REVERB, 22.0), (FX_OUT, -7.4)],
            &[m(0, VIB, PITCH, st(0.5))],
        )),
        pr("Lead Kwadrat", "Lead", &with(
            &[(O1_WAVE, SQR), (O1_SHAPE, 30.0), (O1_LEVEL, 70.0), (O2_WAVE, SQR), (O2_LEVEL, 40.0), (O2_COARSE, -12.0), (O2_FINE, 6.0), (FLT_CUTOFF, 4000.0), (FLT_RES, 25.0), (FLT_ENV, 1.0), (MOD_A, 0.5), (MOD_D, 300.0), (MOD_S, 0.0), (AMP_A, 3.0), (AMP_S, 90.0), (AMP_R, 180.0), (L1_WAVE, 0.0), (L1_RATE, 5.5), (L2_WAVE, 1.0), (L2_RATE, 0.3), (V_MODE, LEGATO), (V_GLIDE, 70.0), (FX_DELAY, 20.0), (FX_DTIME, 2.0), (FX_REVERB, 12.0), (FX_OUT, -11.9)],
            &[m(0, LFO2, SH1, 30.0), m(1, VIB, PITCH, st(0.5))],
        )),
        pr("Lead Gwizdek", "Lead", &with(
            &[(O1_WAVE, SINE), (O1_LEVEL, 75.0), (O2_WAVE, TRI), (O2_LEVEL, 30.0), (O2_COARSE, 12.0), (FLT_CUTOFF, 6000.0), (AMP_A, 12.0), (AMP_S, 100.0), (AMP_R, 220.0), (L1_WAVE, 0.0), (L1_RATE, 5.2), (V_MODE, LEGATO), (V_GLIDE, 90.0), (FX_DELAY, 15.0), (FX_DTIME, 4.0), (FX_REVERB, 25.0), (FX_RSIZE, 2.4), (FX_OUT, -10.5)],
            &[m(0, LFO1, PITCH, st(0.15)), m(1, VIB, PITCH, st(0.4))],
        )),
        pr("Lead Ostry", "Lead", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 75.0), (O2_WAVE, SQR), (O2_LEVEL, 30.0), (O2_COARSE, 7.0), (UNI_VOICES, 3.0), (UNI_DETUNE, 38.0), (UNI_WIDTH, 80.0), (FLT_CUTOFF, 9000.0), (FLT_RES, 20.0), (FLT_DRIVE, 30.0), (FLT_ENV, 1.0), (MOD_D, 200.0), (MOD_S, 0.0), (AMP_A, 2.0), (AMP_S, 90.0), (AMP_R, 160.0), (L1_RATE, 5.5), (V_MODE, MONO), (V_GLIDE, 35.0), (FX_DRIVE, 48.0), (FX_DELAY, 12.0), (FX_DTIME, 2.0), (FX_REVERB, 12.0), (FX_OUT, -10.1)],
            &[m(0, VIB, PITCH, st(0.5))],
        )),
        // ------------------------------------------------ pad
        pr("Pad Ciepły", "Pad", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 65.0), (O2_WAVE, TRI), (O2_LEVEL, 50.0), (UNI_VOICES, 2.0), (UNI_DETUNE, 30.0), (UNI_WIDTH, 80.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 2800.0), (FLT_RES, 15.0), (AMP_A, 620.0), (AMP_D, 1500.0), (AMP_S, 78.0), (AMP_R, 2400.0), (L2_RATE, 0.15), (FX_CHORUS, 40.0), (FX_REVERB, 45.0), (FX_RSIZE, 3.5), (FX_OUT, -6.8)],
            &[m(0, LFO2, CUT, oct(0.4))],
        )),
        pr("Pad Powietrzny", "Pad", &with(
            &[(O1_WAVE, AIR), (O1_SHAPE, 30.0), (O1_LEVEL, 70.0), (O2_WAVE, SAW), (O2_LEVEL, 25.0), (O2_COARSE, 12.0), (UNI_VOICES, 2.0), (UNI_DETUNE, 25.0), (UNI_WIDTH, 90.0), (NOISE_LEVEL, 8.0), (NOISE_COLOR, 60.0), (FLT_CUTOFF, 6000.0), (FLT_RES, 10.0), (AMP_A, 900.0), (AMP_D, 2000.0), (AMP_S, 85.0), (AMP_R, 3000.0), (L2_RATE, 0.12), (FX_CHORUS, 30.0), (FX_REVERB, 55.0), (FX_RSIZE, 5.0), (FX_OUT, -1.0)],
            &[m(0, LFO2, SH1, 40.0)],
        )),
        pr("Pad Ciemny", "Pad", &[(O1_WAVE, HOLLOW), (O1_LEVEL, 70.0), (O2_WAVE, SAW), (O2_LEVEL, 50.0), (O2_COARSE, -12.0), (UNI_VOICES, 1.0), (UNI_DETUNE, 20.0), (UNI_WIDTH, 70.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 900.0), (FLT_RES, 20.0), (AMP_A, 400.0), (AMP_D, 2000.0), (AMP_S, 70.0), (AMP_R, 2000.0), (FX_CHORUS, 25.0), (FX_REVERB, 40.0), (FX_RSIZE, 3.0), (FX_OUT, -5.9)]),
        pr("Pad Ruch", "Pad", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 65.0), (O2_WAVE, SQR), (O2_SHAPE, 30.0), (O2_LEVEL, 35.0), (O2_FINE, 5.0), (UNI_VOICES, 2.0), (UNI_DETUNE, 40.0), (UNI_WIDTH, 100.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 1500.0), (FLT_RES, 28.0), (AMP_A, 700.0), (AMP_D, 1500.0), (AMP_S, 82.0), (AMP_R, 2500.0), (L1_WAVE, 1.0), (L1_SYNC, 3.0), (L2_RATE, 0.2), (FX_CHORUS, 35.0), (FX_REVERB, 45.0), (FX_RSIZE, 4.0), (FX_OUT, -7.6)],
            &[m(0, LFO1, CUT, oct(1.4)), m(1, LFO2, SH1, 35.0)],
        )),
        pr("Pad Supersaw", "Pad", &[(O1_WAVE, SAW), (O1_LEVEL, 70.0), (O2_WAVE, SAW), (O2_LEVEL, 45.0), (O2_COARSE, 12.0), (UNI_VOICES, 3.0), (UNI_DETUNE, 55.0), (UNI_WIDTH, 100.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 5000.0), (FLT_RES, 8.0), (AMP_A, 250.0), (AMP_D, 1500.0), (AMP_S, 85.0), (AMP_R, 1800.0), (FX_CHORUS, 35.0), (FX_REVERB, 40.0), (FX_RSIZE, 3.5), (FX_OUT, -7.9)]),
        // ------------------------------------------------ keys
        pr("Keys Dance", "Klawisze", &[(O1_WAVE, SQR), (O1_SHAPE, 35.0), (O1_LEVEL, 70.0), (O2_WAVE, SAW), (O2_LEVEL, 50.0), (O2_FINE, 8.0), (UNI_VOICES, 1.0), (UNI_DETUNE, 20.0), (UNI_WIDTH, 60.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 1200.0), (FLT_RES, 15.0), (FLT_ENV, 3.0), (MOD_A, 0.5), (MOD_D, 200.0), (MOD_S, 10.0), (AMP_A, 1.5), (AMP_D, 700.0), (AMP_S, 40.0), (AMP_R, 160.0), (FX_DRIVE, 18.0), (FX_CHORUS, 30.0), (FX_REVERB, 16.0), (FX_OUT, -1.9)]),
        pr("Future Chord", "Klawisze", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 70.0), (O2_WAVE, SQR), (O2_LEVEL, 28.0), (O2_COARSE, 12.0), (UNI_VOICES, 3.0), (UNI_DETUNE, 38.0), (UNI_WIDTH, 100.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 1800.0), (FLT_RES, 12.0), (FLT_ENV, 2.5), (MOD_A, 0.5), (MOD_D, 380.0), (MOD_S, 20.0), (AMP_A, 2.0), (AMP_D, 700.0), (AMP_S, 55.0), (AMP_R, 380.0), (FX_DRIVE, 12.0), (FX_CHORUS, 40.0), (FX_DELAY, 16.0), (FX_DTIME, 3.0), (FX_REVERB, 24.0), (FX_RSIZE, 2.2), (FX_OUT, -4.1)],
            &[m(0, VEL, CUT, oct(1.0))],
        )),
        pr("EP Klasyczny", "Klawisze", &with(
            &[(O1_WAVE, SINE), (O1_SHAPE, 25.0), (O1_LEVEL, 80.0), (O2_WAVE, METAL), (O2_LEVEL, 0.0), (O2_COARSE, 24.0), (FLT_CUTOFF, 5000.0), (FLT_KEY, 50.0), (MOD_A, 0.5), (MOD_D, 250.0), (MOD_S, 0.0), (AMP_A, 2.0), (AMP_D, 3000.0), (AMP_S, 30.0), (AMP_R, 400.0), (L1_RATE, 4.5), (FX_CHORUS, 25.0), (FX_REVERB, 20.0), (FX_OUT, -2.0)],
            &[m(0, MENV, L2, 30.0), m(1, VEL, CUT, oct(1.5)), m(2, LFO1, PAN, 25.0)],
        )),
        pr("Organy", "Klawisze", &with(
            &[(O1_WAVE, HOLLOW), (O1_SHAPE, 100.0), (O1_LEVEL, 70.0), (O2_WAVE, SINE), (O2_LEVEL, 40.0), (O2_COARSE, 12.0), (FLT_CUTOFF, 8000.0), (AMP_A, 3.0), (AMP_D, 10.0), (AMP_S, 100.0), (AMP_R, 60.0), (L1_RATE, 6.0), (FX_CHORUS, 45.0), (FX_REVERB, 15.0), (FX_OUT, -10.8)],
            &[m(0, LFO1, AMP, 6.0)],
        )),
        pr("Klawisz Miękki", "Klawisze", &[(O1_WAVE, TRI), (O1_LEVEL, 75.0), (O2_WAVE, SINE), (O2_LEVEL, 35.0), (O2_COARSE, 12.0), (FLT_CUTOFF, 3000.0), (FLT_ENV, 1.0), (MOD_D, 400.0), (MOD_S, 0.0), (AMP_A, 3.0), (AMP_D, 900.0), (AMP_S, 40.0), (AMP_R, 300.0), (FX_CHORUS, 20.0), (FX_REVERB, 20.0), (FX_OUT, -2.2)]),
        // ------------------------------------------------ bells / mallets
        pr("Dzwonek Ciepły", "Dzwonki", &with(
            &[(O1_WAVE, TRI), (O1_SHAPE, 20.0), (O1_LEVEL, 75.0), (O2_WAVE, SINE), (O2_LEVEL, 35.0), (O2_COARSE, 12.0), (O2_FINE, 7.0), (SUB_LEVEL, 18.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 3200.0), (FLT_ENV, 1.0), (MOD_D, 350.0), (MOD_S, 0.0), (AMP_A, 4.0), (AMP_D, 2600.0), (AMP_S, 0.0), (AMP_R, 900.0), (FX_CHORUS, 22.0), (FX_REVERB, 30.0), (FX_RSIZE, 2.6), (FX_OUT, -0.8)],
            &[m(0, MENV, L2, 12.0)],
        )),
        pr("Vibrafon", "Dzwonki", &with(
            &[(O1_WAVE, SINE), (O1_LEVEL, 80.0), (O2_WAVE, SINE), (O2_LEVEL, 18.0), (O2_COARSE, 24.0), (FLT_CUTOFF, 6000.0), (AMP_A, 2.0), (AMP_D, 2800.0), (AMP_S, 0.0), (AMP_R, 700.0), (L1_RATE, 5.0), (FX_CHORUS, 15.0), (FX_REVERB, 28.0), (FX_RSIZE, 2.4), (FX_OUT, -0.6)],
            &[m(0, LFO1, AMP, 10.0)],
        )),
        pr("Marimba", "Dzwonki", &with(
            &[(O1_WAVE, SINE), (O1_LEVEL, 80.0), (O2_WAVE, HOLLOW), (O2_LEVEL, 0.0), (O2_COARSE, 24.0), (FLT_CUTOFF, 4000.0), (MOD_A, 0.5), (MOD_D, 60.0), (MOD_S, 0.0), (AMP_A, 1.0), (AMP_D, 1100.0), (AMP_S, 0.0), (AMP_R, 200.0), (FX_REVERB, 18.0), (FX_OUT, 0.3)],
            &[m(0, MENV, L2, 50.0)],
        )),
        // ------------------------------------------------ brass / strings
        pr("Brass", "Dęte/Smyczki", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 75.0), (O2_WAVE, SAW), (O2_LEVEL, 40.0), (O2_FINE, 8.0), (UNI_VOICES, 1.0), (UNI_DETUNE, 15.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 800.0), (FLT_RES, 10.0), (FLT_KEY, 40.0), (FLT_ENV, 2.5), (MOD_A, 40.0), (MOD_D, 400.0), (MOD_S, 40.0), (MOD_R, 200.0), (AMP_A, 30.0), (AMP_D, 300.0), (AMP_S, 85.0), (AMP_R, 200.0), (L1_RATE, 5.0), (FX_DRIVE, 15.0), (FX_REVERB, 20.0), (FX_OUT, -6.0)],
            &[m(0, VIB, PITCH, st(0.4))],
        )),
        pr("Smyczki", "Dęte/Smyczki", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 75.0), (UNI_VOICES, 3.0), (UNI_DETUNE, 25.0), (UNI_WIDTH, 90.0), (FLT_CUTOFF, 3500.0), (FLT_RES, 5.0), (AMP_A, 350.0), (AMP_D, 1000.0), (AMP_S, 90.0), (AMP_R, 900.0), (L1_RATE, 5.0), (FX_CHORUS, 50.0), (FX_REVERB, 35.0), (FX_RSIZE, 2.8), (FX_OUT, -6.1)],
            &[m(0, LFO1, PITCH, st(0.08))],
        )),
        // ------------------------------------------------ arp
        pr("Arp Cyfrowy", "Arp", &[(FX_DRIVE, 18.0), (O1_WAVE, DIGITAL), (O1_LEVEL, 70.0), (O2_WAVE, SQR), (O2_LEVEL, 35.0), (O2_COARSE, 12.0), (FLT_CUTOFF, 3000.0), (FLT_RES, 15.0), (FLT_ENV, 2.0), (MOD_D, 250.0), (MOD_S, 0.0), (AMP_A, 1.0), (AMP_D, 700.0), (AMP_S, 10.0), (AMP_R, 120.0), (FX_DELAY, 30.0), (FX_DTIME, 3.0), (FX_DFB, 45.0), (FX_REVERB, 15.0), (FX_OUT, -0.2)]),
        pr("Arp Puls", "Arp", &with(
            &[(FX_DRIVE, 12.0), (O1_WAVE, SQR), (O1_LEVEL, 75.0), (FLT_CUTOFF, 2000.0), (FLT_RES, 40.0), (FLT_ENV, 3.0), (MOD_D, 220.0), (MOD_S, 0.0), (AMP_A, 1.0), (AMP_D, 700.0), (AMP_S, 0.0), (AMP_R, 100.0), (L2_RATE, 0.5), (FX_DELAY, 25.0), (FX_DTIME, 0.0), (FX_DFB, 40.0), (FX_REVERB, 12.0), (FX_OUT, -0.9)],
            &[m(0, LFO2, SH1, 40.0), m(1, RND, CUT, oct(0.3))],
        )),
        pr("Arp Trance", "Arp", &[(O1_WAVE, SAW), (O1_LEVEL, 70.0), (O2_WAVE, SQR), (O2_LEVEL, 35.0), (UNI_VOICES, 1.0), (UNI_DETUNE, 25.0), (UNI_WIDTH, 80.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 1400.0), (FLT_RES, 10.0), (FLT_ENV, 3.0), (MOD_A, 0.5), (MOD_D, 220.0), (MOD_S, 0.0), (AMP_A, 1.0), (AMP_D, 750.0), (AMP_S, 0.0), (AMP_R, 90.0), (FX_DRIVE, 15.0), (FX_DELAY, 35.0), (FX_DTIME, 3.0), (FX_DFB, 48.0), (FX_REVERB, 14.0), (FX_OUT, -0.3)]),
        // ------------------------------------------------ lo-fi
        pr("Lo-fi Klawisz", "Lo-fi", &with(
            &[(O1_WAVE, TRI), (O1_LEVEL, 75.0), (O2_WAVE, DIGITAL), (O2_SHAPE, 50.0), (O2_LEVEL, 25.0), (NOISE_LEVEL, 6.0), (NOISE_COLOR, -60.0), (FLT_CUTOFF, 1800.0), (FLT_DRIVE, 25.0), (AMP_A, 5.0), (AMP_D, 1200.0), (AMP_S, 30.0), (AMP_R, 400.0), (L1_WAVE, 1.0), (L1_RATE, 0.7), (FX_CHORUS, 20.0), (FX_REVERB, 25.0), (FX_OUT, -1.5)],
            &[m(0, LFO1, PITCH, st(0.12))],
        )),
        pr("Lo-fi Pad", "Lo-fi", &with(
            &[(O1_WAVE, HOLLOW), (O1_LEVEL, 70.0), (O2_WAVE, AIR), (O2_LEVEL, 40.0), (NOISE_LEVEL, 10.0), (NOISE_COLOR, -60.0), (UNI_VOICES, 1.0), (UNI_DETUNE, 20.0), (FLT_CUTOFF, 1500.0), (AMP_A, 500.0), (AMP_D, 1500.0), (AMP_S, 80.0), (AMP_R, 2000.0), (L1_WAVE, 1.0), (L1_RATE, 0.5), (FX_CHORUS, 35.0), (FX_REVERB, 40.0), (FX_RSIZE, 3.0), (FX_OUT, -3.9)],
            &[m(0, LFO1, PITCH, st(0.15))],
        )),
        // ------------------------------------------------ fx
        pr("Riser", "FX", &with(
            &[(O1_WAVE, SAW), (O1_LEVEL, 60.0), (UNI_VOICES, 3.0), (UNI_DETUNE, 50.0), (UNI_WIDTH, 100.0), (NOISE_LEVEL, 60.0), (NOISE_COLOR, 50.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 300.0), (FLT_RES, 30.0), (FLT_ENV, 6.0), (MOD_A, 4000.0), (MOD_D, 1000.0), (MOD_S, 100.0), (MOD_R, 1500.0), (AMP_A, 3000.0), (AMP_S, 100.0), (AMP_R, 1500.0), (FX_DELAY, 25.0), (FX_DTIME, 2.0), (FX_REVERB, 50.0), (FX_RSIZE, 4.0), (FX_OUT, -2.5)],
            &[m(0, MENV, PITCH, st(12.0))],
        )),
        pr("Zjazd", "FX", &with(
            &[(FX_DRIVE, 50.0), (O1_WAVE, SAW), (O1_LEVEL, 70.0), (UNI_VOICES, 2.0), (UNI_DETUNE, 40.0), (UNI_WIDTH, 100.0), (NOISE_LEVEL, 40.0), (FLT_TYPE, BP), (FLT_CUTOFF, 2500.0), (FLT_RES, 30.0), (FLT_ENV, -3.0), (MOD_A, 0.5), (MOD_D, 1500.0), (MOD_S, 0.0), (AMP_A, 5.0), (AMP_D, 1800.0), (AMP_S, 0.0), (AMP_R, 1000.0), (FX_REVERB, 60.0), (FX_RSIZE, 4.5), (FX_OUT, 6.6)],
            &[m(0, MENV, PITCH, st(-36.0)), m(1, MENV, NOISE, 30.0)],
        )),
        pr("Impact", "FX", &with(
            &[(O1_WAVE, SINE), (O1_LEVEL, 90.0), (SUB_LEVEL, 40.0), (NOISE_LEVEL, 45.0), (NOISE_COLOR, -40.0), (FLT_TYPE, LP24), (FLT_CUTOFF, 6000.0), (FLT_ENV, -3.0), (MOD_A, 0.5), (MOD_D, 900.0), (MOD_S, 0.0), (AMP_A, 0.8), (AMP_D, 1400.0), (AMP_S, 0.0), (AMP_R, 800.0), (FX_DRIVE, 40.0), (FX_REVERB, 45.0), (FX_RSIZE, 3.5), (FX_OUT, -0.6)],
            &[m(0, MENV, PITCH, st(30.0))],
        )),
        pr("Szum Wiatru", "FX", &with(
            &[(O1_LEVEL, 0.0), (NOISE_LEVEL, 100.0), (NOISE_COLOR, 0.0), (FLT_TYPE, BP), (FLT_CUTOFF, 1200.0), (FLT_RES, 60.0), (FLT_KEY, 100.0), (AMP_A, 400.0), (AMP_S, 100.0), (AMP_R, 1500.0), (L1_WAVE, 0.0), (L1_RATE, 0.2), (L2_WAVE, 4.0), (L2_RATE, 3.0), (FX_REVERB, 45.0), (FX_RSIZE, 4.0), (FX_OUT, -6.9)],
            &[m(0, LFO1, CUT, oct(1.5)), m(1, LFO2, CUT, oct(0.3)), m(2, LFO1, PAN, 40.0)],
        )),
    ]
}
