//! The project: everything that is saved, undone, and shared between the GUI and the CLI.
//!
//! Plain data. The engine thread owns the one true copy; the UI and the CLI see
//! JSON snapshots of it and change it only through [`crate::ops::Op`]s.
//!
//! Units, so nobody has to guess:
//! * time on the playlist is in **beats** (quarter notes), `f64`;
//! * pattern notes are in **steps** (sixteenths), `f32`;
//! * fades are in **seconds** (they belong to the audio, not the grid);
//! * `vol` is a **fader position** 0..1, gain = 2·t³ (0.7937 ≈ 0 dB, 1.0 ≈ +6 dB);
//! * clip `gain` is in **dB**, `pan` is -1..1.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type Id = u64;

/// Fader position that gives unity gain.
pub const VOL_UNITY: f32 = 0.7937;
/// Beats shown on the playlist. The arrangement may not go past this.
pub const TOTAL_BEATS: f64 = 512.0;

pub fn fader_gain(t: f32) -> f32 {
    2.0 * t * t * t
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub version: u32,
    pub bpm: f64,
    pub tracks: Vec<Track>,
    /// `inserts[0]` is always the master (id 0).
    pub inserts: Vec<Insert>,
    pub patterns: Vec<Pattern>,
    pub sources: Vec<Source>,
    pub clips: Vec<Clip>,
    #[serde(rename = "loop")]
    pub loop_range: Option<LoopRange>,
    /// Automation envelopes; played by clips of kind `auto`.
    #[serde(default)]
    pub automations: Vec<Automation>,
    /// Next id handed out by [`Project::alloc_id`]. Ids are never reused.
    pub next_id: Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRange {
    pub a: f64,
    pub b: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: Id,
    pub name: String,
    pub color: String,
    pub muted: bool,
    pub solo: bool,
    pub arm: bool,
    pub vol: f32,
    /// Lane height in px. UI state, but it is saved with the song like in FL.
    pub h: f32,
    /// Mixer insert id this track plays into.
    pub insert: Id,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Insert {
    pub id: Id,
    pub name: String,
    pub color: String,
    pub vol: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub fx: Vec<Fx>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FxKind {
    Eq,
    Comp,
    Sat,
    Delay,
    Reverb,
    Limit,
    Fet,
    Deess,
    Tune,
    Denoise,
    Voice,
    Vst3,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fx {
    pub id: Id,
    #[serde(rename = "type")]
    pub kind: FxKind,
    pub bypass: bool,
    /// Dry/wet: 1 = fully processed (default), 0 = dry.
    #[serde(default = "one")]
    pub mix: f32,
    /// Index into the built-in preset list this was last loaded from (UI shows `*` when edited).
    #[serde(default)]
    pub preset: Option<u32>,
    /// Built-in effects: parameter id -> value in the parameter's own unit (see [`crate::fx::params`]).
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
    /// EQ only: up to 24 bands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<Vec<EqBand>>,
    /// VST3 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BandType {
    Lowcut,
    Lowshelf,
    Peak,
    Highshelf,
    Highcut,
    Notch,
    Bandpass,
    Tilt,
}

/// Which part of the stereo image a band works on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    #[default]
    Stereo,
    Left,
    Right,
    Mid,
    Side,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub on: bool,
    #[serde(rename = "type")]
    pub kind: BandType,
    pub f: f64,
    pub g: f64,
    pub q: f64,
    /// Cut filters: 12, 24, 36 or 48 dB/oct.
    #[serde(default = "default_slope")]
    pub slope: u8,
    #[serde(default)]
    pub place: Placement,
    /// Dynamic EQ: when set, the band's gain moves by up to `range` dB
    /// as the band's own level goes over `thr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<DynBand>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DynBand {
    /// dB, −30..30 (negative = duck when loud).
    pub range: f64,
    /// dBFS.
    pub thr: f64,
}

fn one() -> f32 {
    1.0
}

fn default_slope() -> u8 {
    12
}

/// Maximum EQ bands.
pub const MAX_BANDS: usize = 24;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRef {
    pub format: String,
    pub path: String,
    /// VST3 class id (hex) as reported by the scan.
    pub cid: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    /// Opaque plugin state (base64), refreshed on save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Inst {
    Kick,
    Clap,
    Hat,
    Bass,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub inst: Inst,
    /// Start in steps (1/16) from the pattern start.
    pub step: f32,
    /// Length in steps.
    pub len: f32,
    /// MIDI key, for pitched instruments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub id: Id,
    pub name: String,
    pub color: String,
    /// Length in beats.
    pub len: f64,
    pub notes: Vec<Note>,
}

/// An audio file the project uses. Samples live in the engine, not here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub id: Id,
    pub name: String,
    pub color: String,
    /// Where the audio is on disk. Recordings and edits are written next to the project.
    pub path: String,
    /// Seconds.
    pub dur: f64,
    /// Sample rate of the file (the engine resamples on load).
    pub sr: u32,
    pub ch: u16,
    pub peak_db: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipKind {
    Pat,
    Audio,
    Auto,
}

/// What an automation moves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AutoTarget {
    /// A built-in effect parameter (catalog id), or `band:<i>:f|g|q` of an EQ.
    Fx { fx_id: Id, param: String },
    /// An effect's dry/wet.
    Mix { fx_id: Id },
    InsertVol { id: Id },
    InsertPan { id: Id },
    TrackVol { id: Id },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutoPoint {
    /// Beats from the start of the automation.
    pub t: f64,
    /// Normalised 0..1 (log parameters are normalised logarithmically).
    pub v: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Automation {
    pub id: Id,
    pub name: String,
    pub color: String,
    pub target: AutoTarget,
    pub points: Vec<AutoPoint>,
}

impl Automation {
    /// Value at `t` beats (linear between points, held at the ends).
    pub fn value_at(&self, t: f64) -> Option<f32> {
        let p = &self.points;
        let first = p.first()?;
        if t <= first.t {
            return Some(first.v);
        }
        let i = p.partition_point(|q| q.t <= t);
        if i >= p.len() {
            return Some(p[p.len() - 1].v);
        }
        let (a, b) = (p[i - 1], p[i]);
        let f = ((t - a.t) / (b.t - a.t).max(1e-9)) as f32;
        Some(a.v + (b.v - a.v) * f)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: Id,
    /// Track index (lane).
    pub track: u32,
    pub start: f64,
    pub len: f64,
    /// Beats into the source/pattern where the clip begins.
    pub offset: f64,
    pub kind: ClipKind,
    #[serde(rename = "ref")]
    pub source: Id,
    pub muted: bool,
    /// dB.
    pub gain: f32,
    #[serde(default)]
    pub fade_in: f32,
    #[serde(default)]
    pub fade_out: f32,
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub norm: bool,
    #[serde(default)]
    pub rev: bool,
    /// Note-level pitch edits (the pitch editor). Rendered offline, latency-free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tune: Option<ClipTune>,
}

/// Pitch edits of an audio clip. `notes` starts as the analysis' segmentation and
/// is then the user's: split, merge, shift. Times are seconds into the source.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipTune {
    pub notes: Vec<NoteEdit>,
    /// Hand-drawn pitch (pencil): `[seconds, midi]` points, sorted. Within a
    /// drawn run it replaces the note's pitch; gaps > 30 ms end a run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pencil: Vec<[f32; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteEdit {
    pub start: f64,
    pub end: f64,
    /// Pitch move in cents.
    #[serde(default)]
    pub shift: f32,
    /// 0..1: how much of the pitch drift/vibrato inside the note is flattened toward its centre.
    #[serde(default)]
    pub drift: f32,
    /// Formant move in cents (0 = natural).
    #[serde(default)]
    pub formant: f32,
    /// Level change in dB.
    #[serde(default)]
    pub gain: f32,
}

impl Project {
    pub fn alloc_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn insert(&self, id: Id) -> Option<&Insert> {
        self.inserts.iter().find(|m| m.id == id)
    }

    pub fn find_fx(&self, fx_id: Id) -> Option<(&Insert, &Fx)> {
        self.inserts
            .iter()
            .find_map(|m| m.fx.iter().find(|f| f.id == fx_id).map(|f| (m, f)))
    }

    /// End of the last clip, rounded up to a bar; at least one bar.
    pub fn song_end(&self) -> f64 {
        let end = self.clips.iter().map(|c| c.start + c.len).fold(4.0, f64::max);
        (end / 4.0).ceil() * 4.0
    }

    /// A fresh project: the demo song of the prototype minus the audio loops.
    pub fn demo() -> Self {
        crate::demo::demo_project()
    }

    pub fn empty() -> Self {
        crate::demo::empty_project()
    }
}
