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
}

const fn p(id: &'static str, label: &'static str, min: f64, max: f64, def: f64, unit: &'static str, log: bool) -> ParamDef {
    ParamDef { id, label, min, max, def, unit, log }
}

pub fn params(kind: FxKind) -> &'static [ParamDef] {
    const EQ: &[ParamDef] = &[p("out", "Output", -18.0, 18.0, 0.0, "dB", false)];
    const COMP: &[ParamDef] = &[
        p("thr", "Thresh", -60.0, 0.0, -18.0, "dB", false),
        p("ratio", "Ratio", 1.0, 20.0, 4.0, ":1", true),
        p("att", "Attack", 0.1, 100.0, 10.0, "ms", true),
        p("rel", "Release", 10.0, 1000.0, 150.0, "ms", true),
        p("gain", "Makeup", 0.0, 24.0, 0.0, "dB", false),
    ];
    const SAT: &[ParamDef] = &[
        p("drive", "Drive", 0.0, 36.0, 6.0, "dB", false),
        p("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
        p("out", "Output", -24.0, 6.0, 0.0, "dB", false),
    ];
    const DELAY: &[ParamDef] = &[
        p("time", "Time", 10.0, 1500.0, 375.0, "ms", true),
        p("fb", "Feedback", 0.0, 95.0, 35.0, "%", false),
        p("mix", "Mix", 0.0, 100.0, 25.0, "%", false),
    ];
    const REVERB: &[ParamDef] = &[
        p("size", "Decay", 0.3, 8.0, 2.2, "s", true),
        p("pre", "Pre-delay", 0.0, 200.0, 20.0, "ms", false),
        p("lo", "Low cut", 20.0, 1000.0, 120.0, "Hz", true),
        p("hi", "High cut", 1000.0, 20000.0, 8000.0, "Hz", true),
        p("width", "Width", 0.0, 100.0, 100.0, "%", false),
        p("mix", "Mix", 0.0, 100.0, 25.0, "%", false),
    ];
    const LIMIT: &[ParamDef] = &[
        p("gain", "Gain", 0.0, 24.0, 0.0, "dB", false),
        p("ceil", "Ceiling", -12.0, 0.0, -0.3, "dB", false),
    ];
    match kind {
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
        FxKind::Vst3 => "VST3",
    }
}

pub const BUILTIN: [FxKind; 6] = [FxKind::Eq, FxKind::Comp, FxKind::Sat, FxKind::Delay, FxKind::Reverb, FxKind::Limit];

pub fn default_bands() -> Vec<EqBand> {
    use BandType::*;
    let b = |on, kind, f, q| EqBand { on, kind, f, g: 0.0, q };
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
    let mut fx = Fx { id, kind, bypass: false, preset: None, params: default_params(kind), bands: None, plugin: None };
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
            pr("Domyślny", &[("drive", 6.0), ("mix", 100.0), ("out", 0.0)]),
            pr("Subtelny", &[("drive", 2.0), ("mix", 50.0), ("out", 0.0)]),
            pr("Ciepła taśma", &[("drive", 4.0), ("mix", 100.0), ("out", -1.0)]),
            pr("Lampa", &[("drive", 10.0), ("mix", 80.0), ("out", -3.0)]),
            pr("Gruby bas", &[("drive", 18.0), ("mix", 60.0), ("out", -4.0)]),
            pr("Rozwalony", &[("drive", 30.0), ("mix", 100.0), ("out", -8.0)]),
        ],
        FxKind::Delay => vec![
            dly("Ósemka 1/8", Some(0.5), 0.0, 35.0, 20.0),
            dly("Ćwierćnuta 1/4", Some(1.0), 0.0, 30.0, 22.0),
            dly("Kropkowana 1/8", Some(0.75), 0.0, 40.0, 22.0),
            dly("Triola 1/8T", Some(1.0 / 3.0), 0.0, 35.0, 20.0),
            dly("Slapback", None, 90.0, 0.0, 25.0),
            dly("Długi dub", Some(1.5), 0.0, 65.0, 30.0),
        ],
        FxKind::Reverb => vec![
            pr("Domyślny", &[("size", 2.2), ("pre", 20.0), ("lo", 120.0), ("hi", 8000.0), ("width", 100.0), ("mix", 25.0)]),
            pr("Mały pokój", &[("size", 0.6), ("pre", 5.0), ("lo", 150.0), ("hi", 9000.0), ("width", 70.0), ("mix", 18.0)]),
            pr("Bęben — krótki", &[("size", 0.9), ("pre", 0.0), ("lo", 200.0), ("hi", 8000.0), ("width", 60.0), ("mix", 15.0)]),
            pr("Wokal plate", &[("size", 1.8), ("pre", 30.0), ("lo", 250.0), ("hi", 7000.0), ("width", 100.0), ("mix", 22.0)]),
            pr("Duża sala", &[("size", 3.2), ("pre", 25.0), ("lo", 120.0), ("hi", 6500.0), ("width", 100.0), ("mix", 25.0)]),
            pr("Ambient", &[("size", 5.0), ("pre", 120.0), ("lo", 300.0), ("hi", 12000.0), ("width", 100.0), ("mix", 45.0)]),
            pr("Katedra", &[("size", 7.0), ("pre", 60.0), ("lo", 100.0), ("hi", 5000.0), ("width", 100.0), ("mix", 35.0)]),
        ],
        FxKind::Limit => vec![
            pr("Bezpieczny", &[("gain", 0.0), ("ceil", -0.3)]),
            pr("Streaming −1 dB", &[("gain", 3.0), ("ceil", -1.0)]),
            pr("Głośny master", &[("gain", 6.0), ("ceil", -0.3)]),
            pr("Bardzo głośny", &[("gain", 10.0), ("ceil", -0.1)]),
        ],
        FxKind::Vst3 => vec![],
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
    for (i, d) in params(fx.kind).iter().enumerate().take(8) {
        let v = fx.params.get(d.id).copied().unwrap_or(d.def).clamp(d.min, d.max);
        pb.values[i] = v as f32;
    }
    if let Some(bands) = &fx.bands {
        for (i, b) in bands.iter().enumerate().take(7) {
            pb.bands[i] = BandParams {
                on: b.on,
                kind: match b.kind {
                    BandType::Lowcut => 0,
                    BandType::Lowshelf => 1,
                    BandType::Peak => 2,
                    BandType::Highshelf => 3,
                    BandType::Highcut => 4,
                },
                f: b.f.clamp(20.0, 20000.0) as f32,
                g: b.g.clamp(-18.0, 18.0) as f32,
                q: b.q.clamp(0.1, 18.0) as f32,
            };
        }
    }
    pb
}
