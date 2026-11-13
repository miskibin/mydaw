//! The synth's parameters: definitions (the UI catalog), sections, and the resolved
//! fixed-size [`Patch`] the audio thread reads.

use serde::Serialize;

use crate::catalog::ParamDef;
use crate::model::SynthState;

const fn p(id: &'static str, label: &'static str, min: f64, max: f64, def: f64, unit: &'static str, log: bool) -> ParamDef {
    ParamDef { id, label, min, max, def, unit, log, options: None }
}

const fn choice(id: &'static str, label: &'static str, def: f64, options: &'static [&'static str]) -> ParamDef {
    ParamDef { id, label, min: 0.0, max: (options.len() - 1) as f64, def, unit: "", log: false, options: Some(options) }
}

pub const WAVES: &[&str] = &["Sine", "Triangle", "Saw", "Square", "Hollow", "Air", "Digital", "Metal"];
pub const LFO_WAVES: &[&str] = &["Sine", "Triangle", "Saw", "Square", "S&H"];
/// LFO rate: free (Hz) or a note value at the project tempo.
pub const SYNCS: &[&str] =
    &["Hz", "4/1", "2/1", "1/1", "1/2", "1/4", "1/8", "1/16", "1/32", "1/2T", "1/4T", "1/8T", "1/16T", "1/4D", "1/8D"];
/// Beats per LFO cycle for each [`SYNCS`] entry (0 = free).
pub const SYNC_BEATS: [f32; 15] = [0.0, 16.0, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.125, 4.0 / 3.0, 2.0 / 3.0, 1.0 / 3.0, 1.0 / 6.0, 1.5, 0.75];
/// "LFO 1 × wheel": LFO 1 scaled by the mod wheel (vibrato that the wheel brings in).
pub const SOURCES: &[&str] = &["Off", "Mod env", "LFO 1", "LFO 2", "Velocity", "Mod wheel", "Random", "Key", "LFO 1 × wheel"];
pub const DESTS: &[&str] = &[
    "Pitch",
    "Osc 1 pitch",
    "Osc 2 pitch",
    "Osc 1 shape",
    "Osc 2 shape",
    "Osc 1 level",
    "Osc 2 level",
    "Cutoff",
    "Resonance",
    "Pan",
    "Detune",
    "Drive",
    "Amp",
    "Noise",
];
/// What ±100 % of a matrix slot means for each destination (semitones, octaves, or 0..1 units).
pub const DEST_SCALE: [f32; 14] = [48.0, 48.0, 48.0, 1.0, 1.0, 1.0, 1.0, 8.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
pub const FILTERS: &[&str] = &["LP 12", "LP 24", "BP", "HP", "Notch"];
pub const MODES: &[&str] = &["Poly", "Mono", "Legato"];
pub const DELAY_TIMES: &[&str] = &["1/16", "1/8T", "1/8", "1/8D", "1/4", "1/4D", "1/2"];
pub const DELAY_BEATS: [f32; 7] = [0.25, 1.0 / 3.0, 0.5, 0.75, 1.0, 1.5, 2.0];

macro_rules! params {
    ($($ix:ident = $def:expr;)*) => {
        /// Index of every parameter in [`PARAMS`] and in a [`Patch`].
        #[allow(dead_code)]
        pub mod ix { params!(@idx 0usize; $($ix)*); }
        pub static PARAMS: &[ParamDef] = &[$($def),*];
    };
    (@idx $n:expr; $head:ident $($rest:ident)*) => {
        pub const $head: usize = $n;
        params!(@idx $n + 1usize; $($rest)*);
    };
    (@idx $n:expr;) => { pub const COUNT: usize = $n; };
}

params! {
    O1_WAVE = choice("o1_wave", "Wave", 2.0, WAVES);
    O1_SHAPE = p("o1_shape", "Shape", 0.0, 100.0, 0.0, "%", false);
    O1_LEVEL = p("o1_level", "Level", 0.0, 100.0, 80.0, "%", false);
    O1_COARSE = p("o1_coarse", "Coarse", -24.0, 24.0, 0.0, "st", false);
    O2_WAVE = choice("o2_wave", "Wave", 2.0, WAVES);
    O2_SHAPE = p("o2_shape", "Shape", 0.0, 100.0, 0.0, "%", false);
    O2_LEVEL = p("o2_level", "Level", 0.0, 100.0, 0.0, "%", false);
    O2_COARSE = p("o2_coarse", "Coarse", -24.0, 24.0, 0.0, "st", false);
    O2_FINE = p("o2_fine", "Fine", -100.0, 100.0, 0.0, "ct", false);
    SUB_LEVEL = p("sub_level", "Sub", 0.0, 100.0, 0.0, "%", false);
    SUB_OCT = choice("sub_oct", "Sub octave", 0.0, &["-1 oct", "-2 oct"]);
    NOISE_LEVEL = p("noise_level", "Noise", 0.0, 100.0, 0.0, "%", false);
    NOISE_COLOR = p("noise_color", "Colour", -100.0, 100.0, 0.0, "%", false);
    UNI_VOICES = choice("uni_voices", "Unison", 0.0, &["1", "3", "5", "7"]);
    UNI_DETUNE = p("uni_detune", "Detune", 0.0, 100.0, 25.0, "%", false);
    UNI_WIDTH = p("uni_width", "Width", 0.0, 100.0, 70.0, "%", false);
    FLT_TYPE = choice("flt_type", "Type", 0.0, FILTERS);
    FLT_CUTOFF = p("flt_cutoff", "Cutoff", 20.0, 20000.0, 12000.0, "Hz", true);
    FLT_RES = p("flt_res", "Resonance", 0.0, 100.0, 10.0, "%", false);
    FLT_DRIVE = p("flt_drive", "Drive", 0.0, 100.0, 0.0, "%", false);
    FLT_KEY = p("flt_key", "Key track", 0.0, 100.0, 0.0, "%", false);
    FLT_ENV = p("flt_env", "Env amount", -8.0, 8.0, 0.0, "oct", false);
    AMP_A = p("amp_a", "Attack", 0.5, 10000.0, 2.0, "ms", true);
    AMP_D = p("amp_d", "Decay", 1.0, 10000.0, 300.0, "ms", true);
    AMP_S = p("amp_s", "Sustain", 0.0, 100.0, 100.0, "%", false);
    AMP_R = p("amp_r", "Release", 1.0, 10000.0, 200.0, "ms", true);
    MOD_A = p("mod_a", "Attack", 0.5, 10000.0, 1.0, "ms", true);
    MOD_D = p("mod_d", "Decay", 1.0, 10000.0, 300.0, "ms", true);
    MOD_S = p("mod_s", "Sustain", 0.0, 100.0, 0.0, "%", false);
    MOD_R = p("mod_r", "Release", 1.0, 10000.0, 200.0, "ms", true);
    L1_WAVE = choice("l1_wave", "Wave", 0.0, LFO_WAVES);
    L1_RATE = p("l1_rate", "Rate", 0.01, 30.0, 2.0, "Hz", true);
    L1_SYNC = choice("l1_sync", "Sync", 0.0, SYNCS);
    L2_WAVE = choice("l2_wave", "Wave", 0.0, LFO_WAVES);
    L2_RATE = p("l2_rate", "Rate", 0.01, 30.0, 0.5, "Hz", true);
    L2_SYNC = choice("l2_sync", "Sync", 0.0, SYNCS);
    M1_SRC = choice("m1_src", "Source", 0.0, SOURCES);
    M1_DST = choice("m1_dst", "Target", 7.0, DESTS);
    M1_AMT = p("m1_amt", "Amount", -100.0, 100.0, 0.0, "%", false);
    M2_SRC = choice("m2_src", "Source", 0.0, SOURCES);
    M2_DST = choice("m2_dst", "Target", 0.0, DESTS);
    M2_AMT = p("m2_amt", "Amount", -100.0, 100.0, 0.0, "%", false);
    M3_SRC = choice("m3_src", "Source", 0.0, SOURCES);
    M3_DST = choice("m3_dst", "Target", 3.0, DESTS);
    M3_AMT = p("m3_amt", "Amount", -100.0, 100.0, 0.0, "%", false);
    M4_SRC = choice("m4_src", "Source", 0.0, SOURCES);
    M4_DST = choice("m4_dst", "Target", 9.0, DESTS);
    M4_AMT = p("m4_amt", "Amount", -100.0, 100.0, 0.0, "%", false);
    V_MODE = choice("v_mode", "Mode", 0.0, MODES);
    V_GLIDE = p("v_glide", "Glide", 0.0, 2000.0, 0.0, "ms", false);
    FX_DRIVE = p("fx_drive", "Drive", 0.0, 100.0, 0.0, "%", false);
    FX_CHORUS = p("fx_chorus", "Chorus", 0.0, 100.0, 0.0, "%", false);
    FX_DELAY = p("fx_delay", "Delay", 0.0, 100.0, 0.0, "%", false);
    FX_DTIME = choice("fx_dtime", "Delay time", 3.0, DELAY_TIMES);
    FX_DFB = p("fx_dfb", "Feedback", 0.0, 90.0, 35.0, "%", false);
    FX_REVERB = p("fx_reverb", "Reverb", 0.0, 100.0, 0.0, "%", false);
    FX_RSIZE = p("fx_rsize", "Decay", 0.3, 6.0, 1.8, "s", true);
    FX_OUT = p("fx_out", "Output", -30.0, 12.0, 0.0, "dB", false);
}

pub const COUNT: usize = ix::COUNT;

/// Resolved parameter values in [`PARAMS`] order (clamped, defaults filled in).
pub type Patch = [f32; COUNT];

#[derive(Clone, Debug, Serialize)]
pub struct SectionDef {
    pub id: &'static str,
    pub name: &'static str,
    pub params: &'static [ParamDef],
}

/// (id, name, first param, last param + 1): the layout of the synth panel.
const SECTIONS: [(&str, &str, usize, usize); 12] = [
    ("osc1", "Osc 1", ix::O1_WAVE, ix::O2_WAVE),
    ("osc2", "Osc 2", ix::O2_WAVE, ix::SUB_LEVEL),
    ("subnoise", "Sub / Noise", ix::SUB_LEVEL, ix::UNI_VOICES),
    ("unison", "Unison", ix::UNI_VOICES, ix::FLT_TYPE),
    ("filter", "Filter", ix::FLT_TYPE, ix::AMP_A),
    ("ampenv", "Amp env", ix::AMP_A, ix::MOD_A),
    ("modenv", "Mod env", ix::MOD_A, ix::L1_WAVE),
    ("lfo1", "LFO 1", ix::L1_WAVE, ix::L2_WAVE),
    ("lfo2", "LFO 2", ix::L2_WAVE, ix::M1_SRC),
    ("matrix", "Mod matrix", ix::M1_SRC, ix::V_MODE),
    ("voice", "Voice", ix::V_MODE, ix::FX_DRIVE),
    ("fx", "Effects", ix::FX_DRIVE, ix::COUNT),
];

pub fn sections() -> Vec<SectionDef> {
    SECTIONS.iter().map(|&(id, name, a, b)| SectionDef { id, name, params: &PARAMS[a..b] }).collect()
}

pub fn index_of(id: &str) -> Option<usize> {
    PARAMS.iter().position(|d| d.id == id)
}

pub fn defaults() -> Patch {
    let mut p = [0f32; COUNT];
    for (v, d) in p.iter_mut().zip(PARAMS.iter()) {
        *v = d.def as f32;
    }
    p
}

/// Clamps a value into a parameter's range (choices rounded to an option).
pub fn clamp(i: usize, v: f64) -> f32 {
    let d = &PARAMS[i];
    let v = if v.is_finite() { v.clamp(d.min, d.max) } else { d.def };
    (if d.options.is_some() { v.round() } else { v }) as f32
}

/// The patch a channel plays: its parameters over the defaults. Unknown ids are ignored.
pub fn resolve(s: &SynthState) -> Patch {
    let mut p = defaults();
    for (k, v) in &s.params {
        if let Some(i) = index_of(k) {
            p[i] = clamp(i, *v);
        }
    }
    p
}

/// Parameters that glide (smoothed on the audio thread) when changed; the rest switch at once.
pub fn smoothed(i: usize) -> bool {
    let d = &PARAMS[i];
    d.options.is_none()
        && !matches!(
            i,
            ix::V_GLIDE | ix::AMP_A | ix::AMP_D | ix::AMP_R | ix::MOD_A | ix::MOD_D | ix::MOD_R | ix::L1_RATE | ix::L2_RATE | ix::FX_RSIZE
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_match_ids() {
        assert_eq!(PARAMS.len(), COUNT);
        assert_eq!(PARAMS[ix::FLT_CUTOFF].id, "flt_cutoff");
        assert_eq!(PARAMS[ix::FX_OUT].id, "fx_out");
        assert_eq!(PARAMS[ix::M4_AMT].id, "m4_amt");
        let mut ids: Vec<&str> = PARAMS.iter().map(|d| d.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), COUNT, "duplicate ids");
        let total: usize = sections().iter().map(|s| s.params.len()).sum();
        assert_eq!(total, COUNT);
        assert_eq!(DESTS.len(), DEST_SCALE.len());
        assert_eq!(SYNCS.len(), SYNC_BEATS.len());
        assert_eq!(DELAY_TIMES.len(), DELAY_BEATS.len());
    }
}
