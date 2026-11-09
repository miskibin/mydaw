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
    /// Take groups with their comps; played by clips of kind `comp`.
    #[serde(default)]
    pub comps: Vec<Comp>,
    /// Channel Rack rows, shared by every pattern (notes refer to them by `ch`).
    #[serde(default)]
    pub channels: Vec<Channel>,
    /// The lyrics notebook ("Notatnik"): pages of text where one line is one bar.
    #[serde(default)]
    pub notebook: Notebook,
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
    /// Instrument icon id shown in the lane header (UI only); older songs have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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

impl Inst {
    pub const ALL: [Inst; 4] = [Inst::Kick, Inst::Clap, Inst::Hat, Inst::Bass];
    pub fn name(self) -> &'static str {
        match self {
            Inst::Kick => "Kick",
            Inst::Clap => "Clap",
            Inst::Hat => "Hat",
            Inst::Bass => "Bass",
        }
    }
    /// Key a step of this instrument plays (only the bass is pitched).
    pub fn default_root(self) -> u8 {
        match self {
            Inst::Bass => 36,
            _ => SAMPLE_ROOT,
        }
    }
    pub fn color(self) -> &'static str {
        match self {
            Inst::Kick => "#4f8ef7",
            Inst::Clap => "#d9678f",
            Inst::Hat => "#e0a44a",
            Inst::Bass => "#7c8cf8",
        }
    }
}

/// MIDI key at which a sampler channel plays its sample unpitched (C5 in FL's naming).
pub const SAMPLE_ROOT: u8 = 60;

/// What a channel plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChannelSrc {
    /// A built-in instrument.
    Inst { inst: Inst },
    /// A one-shot sampler playing a project source, pitched by the note (`root` = original pitch).
    Sample { source: Id },
}

/// A Channel Rack row. Channels are global to the project (like FL Studio): every pattern
/// can have notes for every channel, and a note says which channel it plays with `ch`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: Id,
    pub name: String,
    pub color: String,
    #[serde(flatten)]
    pub src: ChannelSrc,
    /// Fader position (0.7937 = unity, see [`fader_gain`]).
    #[serde(default = "vol_unity")]
    pub vol: f32,
    /// -1..1.
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    /// Mixer insert the channel plays into. `None`: the insert of the playlist track
    /// the pattern clip is on (how every channel behaved before channels existed).
    #[serde(default)]
    pub insert: Option<Id>,
    /// Key a step plays when the note has no `p`; for a sampler also the key at which
    /// the sample plays at its original pitch.
    #[serde(default = "sample_root")]
    pub root: u8,
}

fn vol_unity() -> f32 {
    VOL_UNITY
}
fn sample_root() -> u8 {
    SAMPLE_ROOT
}

impl Channel {
    pub fn builtin(id: Id, inst: Inst) -> Self {
        Self { id, name: inst.name().into(), color: inst.color().into(), src: ChannelSrc::Inst { inst }, vol: VOL_UNITY, pan: 0.0, mute: false, solo: false, insert: None, root: inst.default_root() }
    }
    /// Channel gain: the fader curve normalised so the unity position is exactly 1.
    pub fn gain(&self) -> f32 {
        fader_gain(self.vol) / fader_gain(VOL_UNITY)
    }
    /// True for channels whose notes have a meaningful pitch (piano roll).
    pub fn pitched(&self) -> bool {
        matches!(self.src, ChannelSrc::Sample { .. } | ChannelSrc::Inst { inst: Inst::Bass })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// Channel id. `0` only in old files, which say `inst` instead (see [`Project::migrate`]).
    #[serde(default)]
    pub ch: Id,
    /// Legacy (before channels): the built-in instrument. Read, never written.
    #[serde(default, skip_serializing)]
    pub inst: Option<Inst>,
    /// Start in steps (1/16) from the pattern start.
    pub step: f32,
    /// Length in steps.
    pub len: f32,
    /// MIDI key; `None` = the channel's `root`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<u8>,
    /// Velocity 0..1 (gain).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub v: f32,
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

impl Note {
    pub fn new(ch: Id, step: f32, len: f32, p: Option<u8>) -> Self {
        Self { ch, inst: None, step, len, p, v: 1.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub id: Id,
    pub name: String,
    pub color: String,
    /// Length in beats. Notes at or after the end are kept but not played or shown.
    pub len: f64,
    pub notes: Vec<Note>,
    /// 0..1: how far every second 16th is delayed (see [`swing_offset`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub swing: f32,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// Swing, in steps, added to a note that starts on an odd 16th (0-based 1, 3, 5 …).
/// `swing` 1 (100 %) delays it by half a step: the off-beat 16th lands at 75 % of its
/// 8th (MPC 75 %); 0.67 is close to a triplet feel (MPC 66 %). Notes off the 16th grid
/// and on even 16ths do not move.
pub fn swing_offset(step: f32, swing: f32) -> f32 {
    let r = step.round();
    if swing <= 0.0 || (step - r).abs() > 1e-3 || (r as i64).rem_euclid(2) != 1 {
        return 0.0;
    }
    0.5 * swing.clamp(0.0, 1.0)
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
    /// Plays a [`Comp`]: `ref` is the comp id, `offset` is in the comp's own beats.
    Comp,
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
    /// Tune to Reference: the base pitch comes from another performance of the
    /// same melody; note edits and pencil then apply on top of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<RefTune>,
}

impl ClipTune {
    /// True when rendering would change the source.
    pub fn is_active(&self) -> bool {
        !self.notes.is_empty() || !self.pencil.is_empty() || self.reference.is_some()
    }
}

/// Parameters of Tune to Reference. The target pitch is derived from them
/// deterministically (`crate::reference`), so only these are stored.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefTune {
    /// Audio source holding the reference vocal (a cappella).
    pub source: Id,
    /// 0..1: how far the pitch moves from the sung one toward the target.
    pub correction: f32,
    /// 0..1: how much of the singer's own vibrato and note transitions is kept.
    pub vibrato: f32,
    /// 0..1: note centres from the reference (1) or from the nearest scale note (0).
    pub influence: f32,
    /// Reference transposition in semitones (a multiple of 12); `None` = automatic.
    #[serde(default)]
    pub octave: Option<i32>,
    /// Key (0 = C) and scale (`pitch::scale_mask`) for the scale part of `influence`.
    #[serde(default)]
    pub key: u32,
    #[serde(default)]
    pub scale: u32,
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

/// Default crossfade at comp region boundaries, seconds.
pub const DEFAULT_XFADE: f32 = 0.010;
/// Longest crossfade a comp boundary may have, seconds.
pub const MAX_XFADE: f32 = 0.5;

/// One recorded pass. Takes are never cut: the comp only chooses which parts play.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Take {
    pub id: Id,
    pub name: String,
    /// Source with the audio.
    #[serde(rename = "ref")]
    pub source: Id,
    /// Beats from the comp's origin (comp time, see [`Comp`]).
    pub start: f64,
    /// Length in beats.
    pub len: f64,
    /// Beats into the source where the take begins.
    #[serde(default)]
    pub offset: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl Take {
    pub fn end(&self) -> f64 {
        self.start + self.len
    }
}

/// A piece of the comp: `take` plays over `[from, to)` (comp beats).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompRegion {
    pub take: Id,
    pub from: f64,
    pub to: f64,
    /// Crossfade at `from` in seconds (the comp default when absent). A region whose
    /// end is followed by silence uses it for its fade-out as well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xfade: Option<f32>,
}

/// The project's lyrics notebook. Text only: nothing here plays or renders, it is saved,
/// undone and shared with the CLI like the rest of the song.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notebook {
    /// Tabs in display order.
    #[serde(default)]
    pub pages: Vec<LyricPage>,
}

/// One notebook page (a verse, a hook, …). Each line of `text` is one bar, counted from
/// `start_bar` (1-based like the playlist ruler); an empty line is a bar of rest, a line
/// wrapped in square brackets (`[Refren]`) is a header and takes no bar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricPage {
    pub id: Id,
    pub name: String,
    #[serde(default = "first_bar")]
    pub start_bar: u32,
    #[serde(default)]
    pub text: String,
}

fn first_bar() -> u32 {
    1
}

/// Limits of a notebook (a bad CLI script must not bloat every undo snapshot).
pub const NOTEBOOK_MAX_PAGES: usize = 64;
pub const NOTEBOOK_MAX_TEXT: usize = 100_000;
pub const NOTEBOOK_MAX_BAR: u32 = 9_999;

/// A take group and its comp. Times are in **comp beats**: a clip of kind `comp`
/// maps them to the timeline like an audio clip maps its source (`offset` = comp
/// beat at the clip's start), so moving, trimming or splitting the clip just works.
/// `regions` are sorted and do not overlap; gaps are silent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comp {
    pub id: Id,
    pub name: String,
    pub takes: Vec<Take>,
    pub regions: Vec<CompRegion>,
    /// Default crossfade at region boundaries, seconds.
    #[serde(default = "default_xfade")]
    pub xfade: f32,
}

fn default_xfade() -> f32 {
    DEFAULT_XFADE
}

impl Comp {
    pub fn take(&self, id: Id) -> Option<&Take> {
        self.takes.iter().find(|t| t.id == id)
    }
    /// Earliest take start and latest take end (comp beats).
    pub fn extent(&self) -> (f64, f64) {
        let a = self.takes.iter().map(|t| t.start).fold(f64::INFINITY, f64::min);
        let b = self.takes.iter().map(|t| t.end()).fold(f64::NEG_INFINITY, f64::max);
        if a.is_finite() {
            (a, b)
        } else {
            (0.0, 0.0)
        }
    }
    /// Puts `take` over `[from, to)`, replacing whatever played there, and merges
    /// neighbouring pieces of the same take. The range is clipped to the take.
    pub fn write(&mut self, take: Id, from: f64, to: f64) {
        let Some(t) = self.take(take) else { return };
        let (from, to) = (from.max(t.start), to.min(t.end()));
        if to - from <= 1e-9 {
            return;
        }
        let mut out = Vec::with_capacity(self.regions.len() + 2);
        for r in &self.regions {
            if r.to <= from || r.from >= to {
                out.push(*r);
                continue;
            }
            if r.from < from {
                out.push(CompRegion { to: from, ..*r });
            }
            if r.to > to {
                out.push(CompRegion { from: to, xfade: None, ..*r });
            }
        }
        out.push(CompRegion { take, from, to, xfade: None });
        out.sort_by(|a, b| a.from.total_cmp(&b.from));
        self.regions = merge_regions(out);
    }
    /// Removes a take and every region that plays it.
    pub fn remove_take(&mut self, id: Id) {
        self.takes.retain(|t| t.id != id);
        self.regions.retain(|r| r.take != id);
    }
}

/// Joins touching regions of the same take (input sorted by `from`).
pub fn merge_regions(sorted: Vec<CompRegion>) -> Vec<CompRegion> {
    let mut merged: Vec<CompRegion> = Vec::with_capacity(sorted.len());
    for r in sorted {
        match merged.last_mut() {
            Some(m) if m.take == r.take && (r.from - m.to).abs() < 1e-9 => m.to = m.to.max(r.to),
            _ => merged.push(r),
        }
    }
    merged
}

impl Project {
    pub fn alloc_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn channel(&self, id: Id) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    /// The channel that plays a legacy `inst` note: the first built-in channel of that
    /// instrument, created (appended) when there is none.
    pub fn channel_for_inst(&mut self, inst: Inst) -> Id {
        if let Some(c) = self.channels.iter().find(|c| c.src == ChannelSrc::Inst { inst }) {
            return c.id;
        }
        let id = self.alloc_id();
        self.channels.push(Channel::builtin(id, inst));
        id
    }

    /// Gives `inst`-only notes (files and CLI scripts from before channels) their channel.
    pub fn resolve_note_channels(&mut self, notes: &mut [Note]) {
        for n in notes.iter_mut() {
            if n.ch == 0 {
                if let Some(inst) = n.inst {
                    n.ch = self.channel_for_inst(inst);
                }
            }
            n.inst = None;
        }
    }

    /// Brings a project from an older file up to date. Old notes said `inst`; they get a
    /// built-in channel each (one per instrument, created in Kick, Clap, Hat, Bass order,
    /// unity volume, centre, routed like before: through the clip's track). A project
    /// without channels gets the four built-ins so the Channel Rack is never empty.
    /// Idempotent; a project that is already current is unchanged.
    pub fn migrate(&mut self) {
        let mut used: Vec<Inst> = vec![];
        for pt in &self.patterns {
            for n in &pt.notes {
                if n.ch == 0 {
                    if let Some(i) = n.inst {
                        if !used.contains(&i) {
                            used.push(i);
                        }
                    }
                }
            }
        }
        for inst in Inst::ALL {
            if used.contains(&inst) {
                self.channel_for_inst(inst);
            }
        }
        let mut pats = std::mem::take(&mut self.patterns);
        for pt in pats.iter_mut() {
            self.resolve_note_channels(&mut pt.notes);
        }
        self.patterns = pats;
        if self.channels.is_empty() {
            for inst in Inst::ALL {
                self.channel_for_inst(inst);
            }
        }
        // ids the file used must stay taken
        let max = self.channels.iter().map(|c| c.id).max().unwrap_or(0);
        if max >= self.next_id {
            self.next_id = max + 1;
        }
    }

    pub fn comp(&self, id: Id) -> Option<&Comp> {
        self.comps.iter().find(|c| c.id == id)
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
