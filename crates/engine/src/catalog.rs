//! Built-in effect types: parameter definitions and factory presets.
//!
//! The UI reads all of this through `fx_catalog`, so a parameter or preset
//! added here appears in the knobs, the preset menu and the CLI at once.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::{BandType, EqBand, Fx, FxKind, Id};
use crate::processor::{BandParams, ParamBlock};

#[derive(Clone, Debug, Serialize)]
pub struct ParamDef {
    pub id: &'static str,
    pub label: &'static str,
    pub min: f64,
    pub max: f64,
    pub def: f64,
    /// "dB", "ms", "s", "%", ":1", "Hz"
    pub unit: &'static str,
    /// Knob travel is logarithmic.
    pub log: bool,
    /// Discrete choice (value = index): shown as buttons, not a knob.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<&'static [&'static str]>,
}

const fn p(id: &'static str, label: &'static str, min: f64, max: f64, def: f64, unit: &'static str, log: bool) -> ParamDef {
    ParamDef { id, label, min, max, def, unit, log, options: None }
}

const fn choice(id: &'static str, label: &'static str, def: f64, options: &'static [&'static str]) -> ParamDef {
    ParamDef { id, label, min: 0.0, max: (options.len() - 1) as f64, def, unit: "", log: false, options: Some(options) }
}

pub fn params(kind: FxKind) -> &'static [ParamDef] {
    const EQ: &[ParamDef] = &[p("out", "Output", -18.0, 18.0, 0.0, "dB", false)];
    const COMP: &[ParamDef] = &[
        p("thr", "Thresh", -60.0, 0.0, -18.0, "dB", false),
        p("ratio", "Ratio", 1.0, 20.0, 4.0, ":1", true),
        p("att", "Attack", 0.1, 100.0, 10.0, "ms", true),
        p("rel", "Release", 10.0, 1000.0, 150.0, "ms", true),
        p("gain", "Makeup", 0.0, 24.0, 0.0, "dB", false),
        choice("det", "Detect", 0.0, &["Peak", "RMS"]),
    ];
    // order = dsp::saturator::idx
    const SAT: &[ParamDef] = &[
        choice("style", "Style", 0.0, &crate::dsp::saturator::STYLES),
        // knob → gain is quadratic: warmth to ~50 %, grit beyond (36 dB at 100 %)
        p("drive", "Drive", 0.0, 100.0, 40.0, "%", false),
        // +20 dB into the stage
        choice("punish", "Punish", 0.0, &["Off", "On"]),
        p("lowcut", "Low cut", 10.0, 1000.0, 10.0, "Hz", true),
        p("thump", "Thump", 0.0, 100.0, 0.0, "%", false),
        p("tone", "Tone", -100.0, 100.0, 0.0, "%", false),
        p("highcut", "High cut", 500.0, 20000.0, 20000.0, "Hz", true),
        choice("steep", "Steep", 0.0, &["Off", "On"]),
        choice("auto", "Auto gain", 1.0, &["Off", "On"]),
        p("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
        p("out", "Output", -24.0, 12.0, 0.0, "dB", false),
        choice("hq", "Oversampling", 0.0, &["2×", "4×"]),
    ];
    const DELAY: &[ParamDef] = &[
        p("time", "Time", 10.0, 1500.0, 375.0, "ms", true),
        p("fb", "Feedback", 0.0, 95.0, 35.0, "%", false),
        p("mix", "Mix", 0.0, 100.0, 25.0, "%", false),
    ];
    // order = dsp::reverb_v2::idx; size/pre/tone/mix are the macros, the rest is "advanced"
    const REVERB: &[ParamDef] = &[
        p("size", "Decay", 0.2, 6.0, 1.0, "s", true),
        p("pre", "Pre-delay", 0.0, 150.0, 25.0, "ms", false),
        p("lo", "Low cut", 20.0, 1000.0, 300.0, "Hz", true),
        p("hi", "High cut", 2000.0, 16000.0, 7000.0, "Hz", true),
        p("width", "Width", 0.0, 100.0, 100.0, "%", false),
        p("mix", "Mix", 0.0, 100.0, 15.0, "%", false),
        p("tone", "Tone", -100.0, 100.0, 0.0, "%", false),
        choice("space", "Space", 0.0, &["Plate", "Room", "Hall", "Ambience"]),
        p("duck", "Ducking", 0.0, 12.0, 0.0, "dB", false),
        p("early", "Early", 0.0, 100.0, 50.0, "%", false),
        p("bass", "Bass decay", 0.5, 1.5, 1.0, "×", false),
        p("mod", "Modulation", 0.0, 100.0, 50.0, "%", false),
    ];
    // order = dsp::limiter_v2::idx
    const LIMIT: &[ParamDef] = &[
        p("gain", "Gain", 0.0, 24.0, 0.0, "dB", false),
        p("ceil", "Ceiling", -12.0, 0.0, -1.0, "dBTP", false),
        p("rel", "Release", 10.0, 1000.0, 80.0, "ms", true),
        choice("auto", "Auto release", 1.0, &["Off", "On"]),
        p("look", "Lookahead", 1.0, 5.0, 3.0, "ms", false),
        p("link", "Stereo link", 0.0, 100.0, 100.0, "%", false),
        p("out", "Output", -12.0, 0.0, 0.0, "dB", false),
    ];
    const FET: &[ParamDef] = &[
        p("input", "Input", 0.0, 36.0, 8.0, "dB", false),
        p("output", "Output", -24.0, 12.0, 0.0, "dB", false),
        choice("ratio", "Ratio", 0.0, &["4", "8", "12", "20", "All"]),
        p("attack", "Attack", 20.0, 800.0, 300.0, "µs", true),
        p("release", "Release", 50.0, 1100.0, 300.0, "ms", true),
        p("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
    ];
    const DEESS: &[ParamDef] = &[
        p("thr", "Threshold", -60.0, 0.0, -32.0, "dB", false),
        p("range", "Range", 0.0, 24.0, 8.0, "dB", false),
        p("freq", "Freq", 2000.0, 16000.0, 6000.0, "Hz", true),
        choice("mode", "Mode", 0.0, &["Split", "Wide"]),
        p("look", "Lookahead", 0.0, 15.0, 5.0, "ms", false),
        choice("listen", "Listen", 0.0, &["Off", "S"]),
        // Relative: the threshold is on the high/low band energy ratio, independent of level
        choice("detect", "Detect", 0.0, &["Absolute", "Relative"]),
    ];
    const TUNE: &[ParamDef] = &[
        choice("key", "Key", 0.0, &["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]),
        choice("scale", "Scale", 0.0, &["Chromatic", "Major", "Minor", "Maj pent", "Min pent"]),
        p("retune", "Retune", 0.0, 400.0, 20.0, "ms", false),
        p("humanize", "Humanize", 0.0, 100.0, 0.0, "%", false),
        p("flex", "Flex", 0.0, 100.0, 0.0, "%", false),
        p("vibrato", "Vibrato", 0.0, 100.0, 0.0, "%", false),
        p("transpose", "Transpose", -12.0, 12.0, 0.0, "st", false),
        p("formant", "Formant", -12.0, 12.0, 0.0, "st", false),
        // like Auto-Tune's input type: the pitch search range, so a loud harmonic can't be taken for the note
        choice("voice", "Voice", 0.0, &["Auto", "Low male", "Male", "Female", "Soprano"]),
    ];
    const DENOISE: &[ParamDef] = &[
        p("reduction", "Reduction", 0.0, 40.0, 12.0, "dB", false),
        p("sens", "Sensitivity", 0.0, 100.0, 50.0, "%", false),
        p("smooth", "Smoothing", 0.0, 100.0, 50.0, "%", false),
        choice("learn", "Profile", 0.0, &["Adaptive", "Hold"]),
        choice("listen", "Listen", 0.0, &["Off", "Noise"]),
    ];
    const VOICE: &[ParamDef] = &[
        choice("mode", "Mode", 0.0, &["Transpose", "Quantize", "Robot"]),
        // Robot: the note the voice is locked to, in semitones from C4
        p("pitch", "Pitch", -12.0, 12.0, 0.0, "st", false),
        p("formant", "Formant", -12.0, 12.0, 0.0, "st", false),
        // formants follow the pitch shift, like a sample played faster or slower
        choice("link", "Link", 0.0, &["Off", "On"]),
        p("drive", "Drive", 0.0, 30.0, 0.0, "dB", false),
        p("out", "Output", -24.0, 12.0, 0.0, "dB", false),
    ];
    // order = rider::idx. Target/floor are what "Auto wokal" measures on the take; with
    // Auto target on, the rider estimates them while playing
    const RIDER: &[ParamDef] = &[
        p("amount", "Amount", 0.0, 100.0, 50.0, "%", false),
        p("phrase", "Phrase", 0.0, 100.0, 20.0, "%", false),
        p("up", "Max boost", 0.0, 12.0, 5.0, "dB", false),
        p("down", "Max cut", 0.0, 12.0, 6.0, "dB", false),
        choice("speed", "Speed", 0.0, &["Sung", "Rap"]),
        choice("auto", "Auto target", 1.0, &["Off", "On"]),
        p("target", "Target", -60.0, 0.0, -24.0, "dB", false),
        p("floor", "Noise floor", -100.0, -20.0, -70.0, "dB", false),
        choice("look", "Look-ahead", 1.0, &["Short", "Long"]),
        p("out", "Output", -24.0, 24.0, 0.0, "dB", false),
        // below this word level the rider idles at 0 dB (breaths, bleed, room)
        p("gate", "Gate", -100.0, -10.0, -60.0, "dB", false),
    ];
    match kind {
        FxKind::Rider => RIDER,
        FxKind::Denoise => DENOISE,
        FxKind::Voice => VOICE,
        FxKind::Fet => FET,
        FxKind::Deess => DEESS,
        FxKind::Tune => TUNE,
        FxKind::Eq => EQ,
        FxKind::Comp => COMP,
        FxKind::Sat => SAT,
        FxKind::Delay => DELAY,
        FxKind::Reverb => REVERB,
        FxKind::Limit => LIMIT,
        FxKind::Vst3 => &[],
    }
}

pub fn type_name(kind: FxKind) -> &'static str {
    match kind {
        FxKind::Eq => "Parametric EQ",
        FxKind::Comp => "Compressor",
        FxKind::Sat => "Saturator",
        FxKind::Delay => "Delay",
        FxKind::Reverb => "Reverb",
        FxKind::Limit => "Limiter",
        FxKind::Fet => "FET Compressor",
        FxKind::Deess => "De-esser",
        FxKind::Tune => "Pitch Correct",
        FxKind::Denoise => "Denoiser",
        FxKind::Voice => "Voice",
        FxKind::Rider => "Vocal Rider",
        FxKind::Vst3 => "VST3",
    }
}

pub const BUILTIN: [FxKind; 12] = [FxKind::Denoise, FxKind::Tune, FxKind::Voice, FxKind::Eq, FxKind::Rider, FxKind::Deess, FxKind::Comp, FxKind::Fet, FxKind::Sat, FxKind::Delay, FxKind::Reverb, FxKind::Limit];

pub fn default_bands() -> Vec<EqBand> {
    use BandType::*;
    let b = |on, kind, f, q| EqBand { on, kind, f, g: 0.0, q, slope: 12, place: Default::default(), dynamic: None };
    vec![
        b(false, Lowcut, 30.0, 0.71),
        b(true, Lowshelf, 90.0, 0.71),
        b(true, Peak, 250.0, 1.0),
        b(true, Peak, 1000.0, 1.0),
        b(true, Peak, 3500.0, 1.0),
        b(true, Highshelf, 9000.0, 0.71),
        b(false, Highcut, 18000.0, 0.71),
    ]
}

pub fn default_params(kind: FxKind) -> BTreeMap<String, f64> {
    params(kind).iter().map(|d| (d.id.to_owned(), d.def)).collect()
}

/// A fresh built-in effect loaded with its first preset.
pub fn new_fx(id: Id, kind: FxKind, bpm: f64) -> Fx {
    let mut fx = Fx { id, kind, bypass: false, mix: 1.0, preset: None, params: default_params(kind), bands: None, plugin: None };
    if kind == FxKind::Eq {
        fx.bands = Some(default_bands());
    }
    if !presets(kind).is_empty() {
        load_preset(&mut fx, 0, bpm);
    }
    fx
}

#[derive(Clone, Debug, Serialize)]
pub struct Preset {
    pub name: &'static str,
    /// Parameter overrides on top of the defaults.
    pub params: BTreeMap<&'static str, f64>,
    /// EQ band overrides: (band index, on, f, g, q); unspecified fields keep the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bands: Option<Vec<EqBand>>,
    /// Delay: tempo-synced time in beats. Resolved to `time` at the current BPM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beats: Option<f64>,
}

fn pr(name: &'static str, kv: &[(&'static str, f64)]) -> Preset {
    Preset { name, params: kv.iter().copied().collect(), bands: None, beats: None }
}

type BandEdit = (usize, Option<bool>, Option<f64>, Option<f64>, Option<f64>);

fn eqp(name: &'static str, out: f64, edits: &[BandEdit]) -> Preset {
    let mut bands = default_bands();
    for &(i, on, f, g, q) in edits {
        let b = &mut bands[i];
        if let Some(v) = on {
            b.on = v;
        }
        if let Some(v) = f {
            b.f = v;
        }
        if let Some(v) = g {
            b.g = v;
        }
        if let Some(v) = q {
            b.q = v;
        }
    }
    Preset { name, params: [("out", out)].into_iter().collect(), bands: Some(bands), beats: None }
}

fn dly(name: &'static str, beats: Option<f64>, time: f64, fb: f64, mix: f64) -> Preset {
    Preset { name, params: [("time", time), ("fb", fb), ("mix", mix)].into_iter().collect(), bands: None, beats }
}

pub fn presets(kind: FxKind) -> Vec<Preset> {
    let (t, f, n): (Option<bool>, Option<bool>, Option<f64>) = (Some(true), None, None);
    match kind {
        FxKind::Eq => vec![
            eqp("Flat", 0.0, &[]),
            eqp("Wokal — obecność", 0.0, &[(0, t, Some(100.0), n, n), (2, f, Some(300.0), Some(-2.5), Some(1.2)), (4, f, Some(3500.0), Some(3.0), Some(0.9)), (5, f, Some(10000.0), Some(2.5), n)]),
            eqp("Kick — punch", 0.0, &[(0, t, Some(30.0), n, n), (1, f, Some(60.0), Some(3.0), n), (2, f, Some(350.0), Some(-4.0), Some(1.4)), (4, f, Some(4000.0), Some(3.0), Some(1.2))]),
            eqp("Bas — czysty dół", 0.0, &[(0, t, Some(35.0), n, n), (1, f, Some(80.0), Some(2.0), n), (2, f, Some(250.0), Some(-3.0), n), (6, t, Some(8000.0), n, n)]),
            eqp("Mud cut", 0.0, &[(0, t, Some(60.0), n, n), (2, f, Some(300.0), Some(-4.0), Some(1.5))]),
            eqp("Air / połysk", 0.0, &[(4, f, Some(5000.0), Some(1.0), n), (5, f, Some(12000.0), Some(4.0), n)]),
            eqp("Telefon / lo-fi", 2.0, &[(0, t, Some(400.0), n, n), (3, f, Some(1500.0), Some(4.0), Some(0.8)), (6, t, Some(3200.0), n, n)]),
            eqp("Master — smile", 0.0, &[(1, f, Some(80.0), Some(1.5), n), (3, f, Some(1000.0), Some(-0.5), Some(0.6)), (5, f, Some(10000.0), Some(1.5), n)]),
        ],
        FxKind::Comp => vec![
            pr("Domyślny", &[("thr", -18.0), ("ratio", 4.0), ("att", 10.0), ("rel", 150.0), ("gain", 0.0)]),
            pr("Wokal — wyrównanie", &[("thr", -20.0), ("ratio", 3.0), ("att", 5.0), ("rel", 120.0), ("gain", 4.0)]),
            pr("Drum bus — glue", &[("thr", -14.0), ("ratio", 2.0), ("att", 30.0), ("rel", 200.0), ("gain", 2.0)]),
            pr("Kick — punch", &[("thr", -16.0), ("ratio", 4.0), ("att", 25.0), ("rel", 80.0), ("gain", 3.0)]),
            pr("Bas — kontrola", &[("thr", -22.0), ("ratio", 5.0), ("att", 3.0), ("rel", 90.0), ("gain", 4.0)]),
            pr("Delikatny", &[("thr", -12.0), ("ratio", 1.5), ("att", 20.0), ("rel", 250.0), ("gain", 1.0)]),
            pr("Smash", &[("thr", -35.0), ("ratio", 12.0), ("att", 0.5), ("rel", 60.0), ("gain", 12.0)]),
        ],
        FxKind::Sat => vec![
            pr("Domyślny", &[("style", 0.0), ("drive", 40.0)]),
            pr("Wokal — ciepło", &[("style", 0.0), ("drive", 35.0), ("lowcut", 80.0), ("tone", 10.0), ("highcut", 16000.0)]),
            pr("Wokal — lampa", &[("style", 3.0), ("drive", 45.0), ("lowcut", 70.0), ("highcut", 15000.0)]),
            pr("Rap — ziarno", &[("style", 1.0), ("drive", 58.0), ("lowcut", 100.0), ("tone", 25.0), ("highcut", 14000.0)]),
            pr("Bębny — smash", &[("style", 1.0), ("drive", 80.0), ("lowcut", 30.0), ("thump", 30.0), ("mix", 70.0)]),
            pr("Bas — waga", &[("style", 2.0), ("drive", 50.0), ("lowcut", 35.0), ("thump", 40.0), ("tone", -20.0)]),
            pr("Lo-fi głośnik", &[("style", 4.0), ("drive", 60.0), ("lowcut", 300.0), ("thump", 35.0), ("tone", 20.0), ("highcut", 4500.0), ("steep", 1.0)]),
            pr("Równoległe zgniecenie", &[("style", 4.0), ("drive", 90.0), ("punish", 1.0), ("mix", 30.0)]),
            pr("Taśma — klej na bus", &[("style", 0.0), ("drive", 22.0), ("highcut", 18000.0)]),
        ],
        FxKind::Delay => vec![
            dly("Ósemka 1/8", Some(0.5), 0.0, 35.0, 20.0),
            dly("Ćwierćnuta 1/4", Some(1.0), 0.0, 30.0, 22.0),
            dly("Kropkowana 1/8", Some(0.75), 0.0, 40.0, 22.0),
            dly("Triola 1/8T", Some(1.0 / 3.0), 0.0, 35.0, 20.0),
            dly("Slapback", None, 90.0, 0.0, 25.0),
            dly("Długi dub", Some(1.5), 0.0, 65.0, 30.0),
        ],
        FxKind::Reverb => {
            let rv = |name, size, pre, lo, hi, width, mix, tone, space, duck, bass| {
                pr(name, &[("size", size), ("pre", pre), ("lo", lo), ("hi", hi), ("width", width), ("mix", mix), ("tone", tone), ("space", space), ("duck", duck), ("early", 50.0), ("bass", bass), ("mod", 50.0)])
            };
            vec![
                rv("Vocal Plate", 1.0, 25.0, 300.0, 7000.0, 100.0, 15.0, 0.0, 0.0, 3.0, 0.8),
                rv("Vocal Room", 0.6, 10.0, 250.0, 8000.0, 90.0, 12.0, -10.0, 1.0, 0.0, 0.9),
                rv("Vocal Hall", 2.0, 35.0, 250.0, 6500.0, 100.0, 14.0, -20.0, 2.0, 4.0, 0.8),
                rv("Ambience", 0.4, 0.0, 200.0, 9000.0, 100.0, 12.0, 10.0, 3.0, 0.0, 1.0),
                rv("Ballada — długi plate", 2.4, 40.0, 350.0, 6000.0, 100.0, 18.0, -30.0, 0.0, 6.0, 0.8),
                rv("Bębny — pokój", 0.7, 0.0, 150.0, 9000.0, 80.0, 12.0, 0.0, 1.0, 0.0, 1.0),
                rv("Wysyłka — hala 100 %", 2.2, 20.0, 200.0, 7000.0, 100.0, 100.0, -10.0, 2.0, 0.0, 0.9),
            ]
        }
        FxKind::Limit => vec![
            pr("Bezpieczny", &[("gain", 0.0), ("ceil", -1.0)]),
            pr("Streaming −1 dBTP", &[("gain", 4.0), ("ceil", -1.0), ("rel", 80.0)]),
            pr("Głośny master", &[("gain", 7.0), ("ceil", -1.0), ("rel", 120.0)]),
            pr("Bardzo głośny", &[("gain", 10.0), ("ceil", -0.5), ("rel", 150.0), ("look", 5.0)]),
            pr("Przezroczysty", &[("gain", 3.0), ("ceil", -1.0), ("rel", 250.0), ("look", 5.0)]),
        ],
        FxKind::Fet => vec![
            pr("Wokal — klasyk", &[("input", 8.0), ("output", -2.0), ("ratio", 0.0), ("attack", 400.0), ("release", 250.0), ("mix", 100.0)]),
            pr("Wokal — z przodu", &[("input", 12.0), ("output", -4.0), ("ratio", 1.0), ("attack", 200.0), ("release", 150.0), ("mix", 100.0)]),
            pr("Bas — kontrola", &[("input", 10.0), ("output", -3.0), ("ratio", 2.0), ("attack", 600.0), ("release", 400.0), ("mix", 100.0)]),
            pr("Równoległa kompresja", &[("input", 18.0), ("output", -6.0), ("ratio", 4.0), ("attack", 100.0), ("release", 120.0), ("mix", 45.0)]),
            pr("All buttons — smash", &[("input", 20.0), ("output", -10.0), ("ratio", 4.0), ("attack", 60.0), ("release", 100.0), ("mix", 100.0)]),
        ],
        FxKind::Deess => vec![
            pr("Wokal — domyślny", &[("thr", -32.0), ("range", 8.0), ("freq", 6000.0), ("mode", 0.0), ("look", 5.0), ("listen", 0.0)]),
            pr("Ostre „s”", &[("thr", -36.0), ("range", 12.0), ("freq", 6500.0), ("mode", 0.0), ("look", 8.0), ("listen", 0.0)]),
            pr("Delikatnie", &[("thr", -26.0), ("range", 5.0), ("freq", 7000.0), ("mode", 0.0), ("look", 5.0), ("listen", 0.0)]),
            pr("Wokal żeński", &[("thr", -32.0), ("range", 8.0), ("freq", 8000.0), ("mode", 0.0), ("look", 6.0), ("listen", 0.0)]),
            pr("Wokal męski", &[("thr", -32.0), ("range", 8.0), ("freq", 5000.0), ("mode", 0.0), ("look", 6.0), ("listen", 0.0)]),
            pr("Szerokopasmowy", &[("thr", -30.0), ("range", 6.0), ("freq", 6000.0), ("mode", 1.0), ("look", 5.0), ("listen", 0.0)]),
        ],
        FxKind::Denoise => vec![
            pr("Wokal — delikatnie", &[("reduction", 8.0), ("sens", 35.0), ("smooth", 50.0)]),
            pr("Wokal — szum pokoju", &[("reduction", 14.0), ("sens", 50.0), ("smooth", 60.0)]),
            pr("Telefon / dyktafon", &[("reduction", 20.0), ("sens", 65.0), ("smooth", 70.0)]),
            pr("Mocno", &[("reduction", 30.0), ("sens", 80.0), ("smooth", 80.0)]),
        ],
        FxKind::Tune => vec![
            pr("Naturalny", &[("retune", 40.0), ("humanize", 30.0), ("flex", 40.0), ("vibrato", 60.0)]),
            pr("Delikatna korekta", &[("retune", 80.0), ("humanize", 50.0), ("flex", 70.0), ("vibrato", 80.0)]),
            pr("Pop — mocno", &[("retune", 10.0), ("humanize", 10.0), ("flex", 10.0), ("vibrato", 30.0)]),
            pr("Efekt robota", &[("retune", 0.0)]),
        ],
        FxKind::Voice => vec![
            pr("Domyślny", &[]),
            pr("Głębszy, pełniejszy", &[("formant", -2.0)]),
            pr("Oktawa w dół", &[("pitch", -12.0), ("formant", -2.0)]),
            pr("Oktawa w górę", &[("pitch", 12.0), ("formant", 2.0)]),
            pr("Zmiana płci ↓", &[("pitch", -4.0), ("formant", -4.0)]),
            pr("Zmiana płci ↑", &[("pitch", 4.0), ("formant", 4.0)]),
            pr("Chipmunk", &[("pitch", 7.0), ("link", 1.0)]),
            pr("Demon", &[("pitch", -7.0), ("formant", -6.0), ("drive", 12.0), ("out", -4.0)]),
            pr("Hard tune + drive", &[("mode", 1.0), ("drive", 10.0), ("out", -3.0)]),
            pr("Robot", &[("mode", 2.0), ("drive", 6.0), ("out", -2.0)]),
        ],
        FxKind::Rider => vec![
            pr("Wokal — śpiew", &[("amount", 50.0), ("phrase", 20.0), ("up", 5.0), ("down", 6.0), ("speed", 0.0), ("auto", 1.0), ("look", 1.0)]),
            pr("Rap", &[("amount", 50.0), ("phrase", 20.0), ("up", 5.0), ("down", 6.0), ("speed", 1.0), ("auto", 1.0), ("look", 1.0)]),
            pr("Delikatnie", &[("amount", 30.0), ("phrase", 10.0), ("up", 3.0), ("down", 4.0), ("speed", 0.0), ("auto", 1.0), ("look", 1.0)]),
            pr("Mocno", &[("amount", 80.0), ("phrase", 30.0), ("up", 8.0), ("down", 9.0), ("speed", 0.0), ("auto", 1.0), ("look", 1.0)]),
            pr("Na żywo (krótki look-ahead)", &[("amount", 50.0), ("phrase", 20.0), ("up", 5.0), ("down", 6.0), ("speed", 0.0), ("auto", 1.0), ("look", 0.0)]),
        ],
        FxKind::Vst3 => vec![],
    }
}

/// Delay an effect adds, in samples (for latency compensation).
pub fn latency(fx: &Fx, sample_rate: f32) -> usize {
    if fx.bypass {
        return 0;
    }
    match fx.kind {
        FxKind::Tune | FxKind::Voice => crate::vocal::tune_latency(sample_rate),
        FxKind::Sat => crate::dsp::saturator::latency_samples(fx.params.get("hq").copied().unwrap_or(0.0) >= 0.5),
        FxKind::Denoise => crate::denoise::latency(sample_rate),
        FxKind::Rider => crate::rider::latency_samples(fx.params.get("speed").copied().unwrap_or(0.0) as f32, fx.params.get("look").copied().unwrap_or(1.0) as f32, sample_rate),
        // look-ahead delays the audio so the gain is already down when the "s" arrives
        FxKind::Limit => crate::dsp::limiter_v2::latency_samples(fx.params.get("look").copied().unwrap_or(3.0) as f32, sample_rate),
        FxKind::Deess => ((fx.params.get("look").copied().unwrap_or(5.0).clamp(0.0, 15.0) / 1000.0) * sample_rate as f64) as usize,
        _ => 0,
    }
}

/// Loads preset `index` into `fx` (tempo-synced delay times resolved at `bpm`).
pub fn load_preset(fx: &mut Fx, index: usize, bpm: f64) -> bool {
    let list = presets(fx.kind);
    let Some(pr) = list.get(index) else { return false };
    fx.params = default_params(fx.kind);
    for (k, v) in &pr.params {
        fx.params.insert((*k).to_owned(), *v);
    }
    if let Some(beats) = pr.beats {
        fx.params.insert("time".into(), (beats * 60000.0 / bpm).clamp(10.0, 1500.0));
    }
    if let Some(b) = &pr.bands {
        fx.bands = Some(b.clone());
    }
    fx.preset = Some(index as u32);
    true
}

#[derive(Serialize)]
pub struct TypeDef {
    pub name: &'static str,
    pub params: &'static [ParamDef],
}

#[derive(Serialize)]
pub struct Catalog {
    pub types: BTreeMap<FxKind, TypeDef>,
    pub presets: BTreeMap<FxKind, Vec<Preset>>,
}

pub fn catalog() -> Catalog {
    Catalog {
        types: BUILTIN.iter().map(|&k| (k, TypeDef { name: type_name(k), params: params(k) })).collect(),
        presets: BUILTIN.iter().map(|&k| (k, presets(k))).collect(),
    }
}

/// Resolves an effect's parameters to the fixed block the DSP reads.
pub fn param_block(fx: &Fx) -> ParamBlock {
    let mut pb = ParamBlock::default();
    for (i, d) in params(fx.kind).iter().enumerate().take(12) {
        let v = fx.params.get(d.id).copied().unwrap_or(d.def).clamp(d.min, d.max);
        pb.values[i] = v as f32;
    }
    if let Some(bands) = &fx.bands {
        for (i, b) in bands.iter().enumerate().take(crate::model::MAX_BANDS) {
            pb.bands[i] = BandParams {
                on: b.on,
                kind: match b.kind {
                    BandType::Lowcut => 0,
                    BandType::Lowshelf => 1,
                    BandType::Peak => 2,
                    BandType::Highshelf => 3,
                    BandType::Highcut => 4,
                    BandType::Notch => 5,
                    BandType::Bandpass => 6,
                    BandType::Tilt => 7,
                },
                f: b.f.clamp(10.0, 22000.0) as f32,
                g: b.g.clamp(-30.0, 30.0) as f32,
                q: b.q.clamp(0.025, 40.0) as f32,
                stages: (b.slope / 12).clamp(1, 4),
                place: b.place as u8,
                dynamic: b.dynamic.is_some(),
                range: b.dynamic.map(|d| d.range.clamp(-30.0, 30.0) as f32).unwrap_or(0.0),
                thr: b.dynamic.map(|d| d.thr.clamp(-80.0, 0.0) as f32).unwrap_or(0.0),
            };
        }
    }
    pb
}
