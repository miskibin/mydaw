//! The `.mydaw` project file: a session, written down.
//!
//! Everything MYDAW has built so far is *live* state -- a [`Project`] behind a
//! mutex, a [`Pattern`] in the sequencer service, a tempo, a block of mixer
//! atomics, and a handful of plugin instances that only the main thread may
//! touch. This module is the one place that knows how to turn all of that into
//! bytes and back.
//!
//! ```text
//!   live session                       .mydaw (JSON)
//!   ------------                       -------------
//!   Project.tracks[i] ---------------> tracks[i] { id, name, source, gain_db, ... }
//!   ClapHost::save_state(track i) ---> tracks[i].state_blob  (base64)
//!   SequencerService::pattern() -----> pattern
//!   SequencerService::bpm() ---------> bpm
//!   Project.master_gain_db ----------> master_gain_db
//!   the master insert ---------------> master_effect { kind, ..., state_blob }
//! ```
//!
//! # Why JSON, and why it is pretty-printed
//! A project file is the user's document. It outlives the build that wrote it,
//! it lands in version control, and when something goes wrong with it the first
//! useful move is to *look at it*. A binary format would be smaller and would
//! make every one of those things worse. The cost is real -- a plugin state blob
//! is base64, which is a third larger than the bytes it carries -- and it is
//! worth paying, because the alternative to "a user can read their own project"
//! is "a user files a bug we cannot reproduce".
//!
//! # Why the field order is fixed, and what depends on it
//! `serde_json` writes a struct's fields in declaration order, so the order in
//! this file *is* the order on disk. That is what makes the round trip
//! **byte-identical**: save, load, save again, and the two files compare equal
//! byte for byte. That property is not decoration. It is what lets a project
//! file be diffed, and it is what makes the round-trip test able to assert
//! "nothing was lost" in one line instead of field by field for ever.
//!
//! [`FORMAT_VERSION`] is therefore declared first, so that a human opening the
//! file -- or a future MYDAW reading only the first line of it -- sees the
//! version before anything else.
//!
//! # Forward migration
//! Three rules, all implemented here:
//!
//! 1. **Unknown fields are ignored.** A file written by a later MYDAW that
//!    added, say, `tracks[].colour` still loads in this build, minus the colour.
//!    That is serde's default and it is deliberately not overridden.
//! 2. **A newer `format_version` is refused by name.** Ignoring unknown fields
//!    is safe; *misreading* known ones is not, and a format that renumbered its
//!    tracks would do exactly that. [`ProjectFileError::FutureVersion`] says so
//!    in a sentence a status bar can show.
//! 3. **An older `format_version` is migrated, never rejected.** A project a
//!    user saved is a document, and a DAW that cannot open its own last release's
//!    files is a DAW people stop trusting with their work.
//!
//! The version is read before anything else is parsed, so a file from the future
//! is refused with the version message rather than with whatever type error its
//! new shape happens to produce first.
//!
//! ## Version 1 -> version 2 (step 9)
//! Version 2 added the piano roll: `pattern.notes`, and a `pattern.steps` that
//! may be 32 or 64 as well as 16. The migration is therefore *nothing* --
//! `notes` is `#[serde(default)]`, so a version 1 file parses as a version 2 one
//! with an empty roll and a one-bar pattern, which is precisely what it meant.
//!
//! What [`parse_project`] does do is stamp [`FORMAT_VERSION`] onto the parsed
//! file, so that the next save writes version 2 rather than silently re-writing
//! a version number this build no longer produces. That is the whole of the
//! migration policy: **migrate on load, write current on save**, and let the
//! byte-identity round trip test cover the rest. `tests/fixtures/v1.mydaw` is a
//! real file written by the step 8 build, kept so that the claim is tested
//! against the format rather than against this build's idea of it.
//!
//! # What a missing plugin does *not* do
//! Nothing in this module fails because a plugin binary is absent -- it does not
//! look at the filesystem at all. A [`SourceEntry::Clap`] whose bundle is gone
//! parses perfectly well; it is [`crate::project_host`] that discovers the
//! absence when it tries to create the instance, and leaves the track silent
//! with a warning. **The reference stays in the file either way**, so saving a
//! project on a machine that lacks a plugin does not throw the plugin away.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use mydaw_plugin_effects::EffectRegistry;
use mydaw_plugin_host_vst3::ClassId;
use mydaw_plugin_sequencer::{
    AudioClip, Clip, Pattern, PatternClip, PatternError, PatternSet, PlayMode, Playlist,
    PlaylistError, PlaylistTrack,
};
use mydaw_rt_core::{StripId, MAX_BPM, MAX_BUSES, MAX_GAIN_DB, MAX_TRACKS, MIN_BPM, SILENCE_DB};
use serde::{Deserialize, Serialize};

use crate::clap_bridge::{from_base64, to_base64};
use crate::dto::{ChannelClipDto, MelodicNoteDto, PatternDto, StepDto, StepRowDto};
use crate::effects::InsertDef;
use crate::project::{check_gain_db, check_pan, BusDef, Project, SendDef, TrackDef};
use crate::source::{ClapSourceRef, Source, Vst3SourceRef};

/// The format this build writes, and the highest it will read.
///
/// | Version | Shipped in | What changed |
/// | --- | --- | --- |
/// | 1 | step 8 | The format. |
/// | 2 | step 9 | `pattern.notes`; `pattern.steps` may be 32 or 64. |
/// | 3 | patterns + playlist | `patterns`, `active_pattern`, `playlist`, `audio_sources`, `play_mode`; the single `pattern` is gone. |
/// | 4 | mixer v2 | `buses`, `sends`, `tracks[].inserts`, `master_inserts`. |
///
/// # Why version 4 is a bump and not just three new optional fields
/// Rule 1 (unknown fields are ignored) already lets an older build open a file
/// with them. That is exactly the problem: a version 3 build opening a project
/// whose drum bus carries a compressor would play it back **without the
/// compressor and without the bus**, silently, and then save that loss back over
/// the user's file. An insert chain and a routing graph are required for correct
/// playback, which is step 8's own stated bump condition, so refusing the file
/// by name is the correct outcome.
pub const FORMAT_VERSION: u32 = 4;

/// File extension, without the dot. The native dialogs filter on it.
pub const PROJECT_EXTENSION: &str = "mydaw";

/// What a file with no name is called in the window title.
pub const UNTITLED: &str = "Untitled";

// ---------------------------------------------------------------------------
// The format
// ---------------------------------------------------------------------------

/// A whole session on disk.
///
/// Field order here is the field order in the file -- see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    /// Always first, so a version gate can be applied before anything else is
    /// believed. See [`FORMAT_VERSION`].
    pub format_version: u32,
    /// Session tempo, in beats per minute.
    pub bpm: f64,
    /// The master fader, in dB.
    pub master_gain_db: f32,
    /// Which track the keyboard piano and the source picker address.
    pub selected_track: usize,
    /// The mixer, in track order. A track's **position** is its note channel,
    /// which is why the order is data rather than presentation.
    pub tracks: Vec<TrackEntry>,
    /// The buses, in strip order. A bus's **position** is what a send names.
    ///
    /// Absent in a version 3 file, which had none -- so the default is an empty
    /// list, which is exactly what such a file meant.
    #[serde(default)]
    pub buses: Vec<BusEntry>,
    /// Every send tap.
    #[serde(default)]
    pub sends: Vec<SendEntry>,
    /// The one plugin on the master insert, or `null`.
    pub master_effect: Option<EffectEntry>,
    /// Internal effects on the master bus, ahead of [`Self::master_effect`].
    #[serde(default)]
    pub master_inserts: Vec<InsertEntry>,
    /// Whether the transport was left on `pat` or `song`.
    #[serde(default = "default_play_mode")]
    pub play_mode: String,
    /// Every pattern, in picker order. The same shape the webview exchanges, so
    /// there is exactly one pattern serialiser in the app rather than two that
    /// can drift.
    ///
    /// Empty in a version 1 or 2 file, which carried [`Self::pattern`] instead;
    /// [`parse_project`] fills it in.
    #[serde(default)]
    pub patterns: Vec<PatternDto>,
    /// Id of the pattern that was being edited.
    #[serde(default)]
    pub active_pattern: u64,
    /// The arrangement.
    #[serde(default)]
    pub playlist: PlaylistEntry,
    /// Every audio file the arrangement references, by path.
    ///
    /// A table rather than a path on each clip, because two clips of the same
    /// take are the common case and a path repeated per clip is a path that can
    /// disagree with itself. Paths are stored as written; a file that has moved
    /// is a warning on load, not a refusal -- exactly as a missing plugin is.
    #[serde(default)]
    pub audio_sources: Vec<AudioSourceEntry>,
    /// The single pattern a version 1 or 2 file carried.
    ///
    /// Read on the way in and **never written**: [`ProjectFile::capture`] fills
    /// [`Self::patterns`], and this stays `None` so a version 3 file has no
    /// vestigial field. See [`migrate_single_pattern`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<LegacyPatternDto>,
}

/// What [`ProjectFile::play_mode`] is when a file does not say. Older files
/// predate the switch, and pattern mode is what they were playing.
fn default_play_mode() -> String {
    PlayMode::Pattern.id().to_owned()
}

/// The arrangement, on disk.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaylistEntry {
    #[serde(default)]
    pub tracks: Vec<PlaylistTrackEntry>,
    #[serde(default)]
    pub clips: Vec<ClipEntry>,
}

/// One lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistTrackEntry {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub muted: bool,
}

/// One clip.
///
/// Flat, with `kind` deciding which of the optional halves is meaningful --
/// the same shape [`ClipDto`](crate::dto::ClipDto) uses on the wire, and for
/// the same reason: an externally-tagged enum would put the discriminant in a
/// wrapper object and make the file harder to read by eye than it needs to be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipEntry {
    pub id: u64,
    /// `pattern` or `audio`. Anything else is skipped on load, with a warning:
    /// a clip kind this build does not know is a *later* format's, and the rest
    /// of the arrangement still opens.
    pub kind: String,
    pub track: usize,
    /// Absolute song ticks.
    pub start: u64,
    pub length: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_frames: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f32>,
}

impl ClipEntry {
    /// One clip, on its way to disk.
    pub fn capture(clip: &Clip) -> Self {
        let base = Self {
            id: clip.id(),
            kind: clip.kind().to_owned(),
            track: clip.track(),
            start: clip.start_ticks(),
            length: clip.length_ticks(),
            pattern_id: None,
            source_id: None,
            offset_frames: None,
            gain_db: None,
        };
        match clip {
            Clip::Pattern(pattern) => Self {
                pattern_id: Some(pattern.pattern_id),
                ..base
            },
            Clip::Audio(audio) => Self {
                source_id: Some(audio.source_id),
                offset_frames: Some(audio.offset_frames),
                gain_db: Some(audio.gain_db),
                ..base
            },
        }
    }

    /// One clip, on its way back.
    ///
    /// `None` -- with a warning -- for a clip this build cannot make sense of:
    /// a kind it does not know, or one missing the half its kind needs. A
    /// later format's clip is not a corrupt file, and the rest of the
    /// arrangement still opens, which is the same answer a missing plugin gets.
    pub fn restore(&self) -> Option<Clip> {
        match self.kind.as_str() {
            "pattern" => match self.pattern_id {
                Some(pattern_id) => Some(Clip::Pattern(PatternClip {
                    id: self.id,
                    pattern_id,
                    track: self.track,
                    start_ticks: self.start,
                    length_ticks: self.length,
                })),
                None => {
                    log::warn!(
                        "clip {} says 'pattern' but names none; skipping it",
                        self.id
                    );
                    None
                }
            },
            "audio" => match self.source_id {
                Some(source_id) => Some(Clip::Audio(AudioClip {
                    id: self.id,
                    source_id,
                    track: self.track,
                    start_ticks: self.start,
                    offset_frames: self.offset_frames.unwrap_or(0),
                    length_ticks: self.length,
                    gain_db: self.gain_db.filter(|db| db.is_finite()).unwrap_or(0.0),
                })),
                None => {
                    log::warn!(
                        "clip {} says 'audio' but names no file; skipping it",
                        self.id
                    );
                    None
                }
            },
            other => {
                log::warn!(
                    "clip {} is of kind '{other}', which this build does not know; skipping it",
                    self.id
                );
                None
            }
        }
    }
}

/// One audio file the arrangement references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSourceEntry {
    pub id: u64,
    /// Where the file was when the project was saved.
    pub path: String,
}

/// The single pattern a version 1 or 2 file carried.
///
/// Kept as its own type rather than by making the current [`PatternDto`] read
/// both shapes: a migration is a *translation between two formats*, and a
/// deserialiser that quietly accepts either is a format with no version at all.
/// This type is frozen -- nothing may be added to it, because nothing will ever
/// write it again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyPatternDto {
    pub steps: u8,
    pub rows: Vec<LegacyRowDto>,
    /// Absent in version 1, which had no piano roll.
    #[serde(default)]
    pub notes: Vec<LegacyNoteDto>,
}

/// One row of the version 1/2 step grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyRowDto {
    pub key: u8,
    #[serde(default)]
    pub channel: u8,
    #[serde(default)]
    pub label: String,
    pub steps: Vec<StepDto>,
}

/// One note of the version 2 piano roll.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LegacyNoteDto {
    pub key: u8,
    #[serde(default)]
    pub channel: u8,
    pub start: u32,
    pub length: u32,
    #[serde(default = "legacy_velocity")]
    pub velocity: f32,
}

fn legacy_velocity() -> f32 {
    mydaw_plugin_sequencer::DEFAULT_STEP_VELOCITY
}

/// Turn a version 1/2 pattern into the version 3 shape.
///
/// The old format had a flat list of rows and a flat list of notes, each entry
/// carrying its own channel; the new one keys by channel. So this buckets both
/// lists by channel, and where a channel had **more than one row** -- legal in
/// the old format, and what a drum-kit track looked like -- every row after the
/// first becomes *notes*.
///
/// That is not a workaround, it is the same translation
/// [`Pattern::compile`](mydaw_plugin_sequencer::Pattern::compile) does: a step
/// is a note at `index * TICKS_PER_STEP` lasting `STEP_GATE_TICKS`. The
/// migrated pattern therefore compiles to the same events the old one did, note
/// for note, which is the only property a migration has to have.
///
/// The pattern is named "Pattern 1" and given id 1, because a version 2 file
/// had exactly one and it had no name.
pub fn migrate_single_pattern(legacy: &LegacyPatternDto) -> PatternDto {
    use mydaw_plugin_sequencer::{STEP_GATE_TICKS, TICKS_PER_STEP};

    let mut channels: Vec<ChannelClipDto> = Vec::new();
    let clip_for = |channels: &mut Vec<ChannelClipDto>, channel: u8| -> usize {
        if let Some(index) = channels.iter().position(|c| c.channel == channel) {
            return index;
        }
        channels.push(ChannelClipDto {
            channel,
            mode: String::new(),
            steps: None,
            notes: Vec::new(),
        });
        channels.len() - 1
    };

    for row in &legacy.rows {
        let index = clip_for(&mut channels, row.channel);
        if channels[index].steps.is_none() {
            channels[index].steps = Some(StepRowDto {
                key: row.key,
                label: String::new(),
                steps: row.steps.clone(),
            });
            continue;
        }
        // A second row on the same channel: the grid cannot hold it, so it
        // becomes the notes it always meant.
        for (step, cell) in row.steps.iter().enumerate() {
            if !cell.on {
                continue;
            }
            channels[index].notes.push(MelodicNoteDto {
                key: row.key,
                start: step as u32 * TICKS_PER_STEP,
                length: STEP_GATE_TICKS,
                velocity: cell.velocity,
            });
        }
    }

    for note in &legacy.notes {
        let index = clip_for(&mut channels, note.channel);
        channels[index].notes.push(MelodicNoteDto {
            key: note.key,
            start: note.start,
            length: note.length,
            velocity: note.velocity,
        });
    }

    PatternDto {
        id: 1,
        name: "Pattern 1".to_owned(),
        color: None,
        steps: legacy.steps,
        channels,
    }
}

/// One track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackEntry {
    /// Stable across renumbering. Restored rather than regenerated, so a track
    /// keeps its identity across a save/load.
    pub id: u32,
    pub name: String,
    pub source: SourceEntry,
    /// The fader, in dB. Stored in dB and not as a multiplier because -6.0 is
    /// exact and 0.5011872 is a rounding of it -- see [`crate::project`].
    pub gain_db: f32,
    pub pan: f32,
    pub muted: bool,
    pub soloed: bool,
    /// The hosted instrument's own state, base64 of whatever the plugin wrote.
    ///
    /// Empty for a built-in source, and empty for a hosted plugin that
    /// implements no state extension -- which is a normal answer, not a failure.
    /// Base64 rather than a JSON array of numbers: four characters per three
    /// bytes instead of four per byte.
    #[serde(default)]
    pub state_blob: String,
    /// Internal effects, in signal order.
    #[serde(default)]
    pub inserts: Vec<InsertEntry>,
    /// Channel pitch in cents. Default 0 so older files stay at recorded pitch.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub pitch_cents: f32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub sample_start_frames: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub sample_end_frames: u64,
}

fn is_zero_f32(value: &f32) -> bool {
    *value == 0.0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// One internal effect on a chain.
///
/// # Why every parameter is written, not only the ones that were moved
/// A default that changes in a later build must not silently change a saved
/// project. Writing the whole set costs a few hundred bytes per effect and makes
/// the file say exactly what it will sound like. It is also the precedent the
/// v1 -> v2 migration set, where `"notes": []` is written explicitly rather than
/// omitted.
///
/// The map is a [`BTreeMap`] so the key order is deterministic and the
/// byte-identical round trip still holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsertEntry {
    /// Stable identity, restored rather than regenerated so a UI list keyed on
    /// it does not look like every effect being replaced after a load.
    pub id: u32,
    /// Which effect, by the `type_id` its factory answers to. An effect this
    /// build does not have is a **warning**, not a refusal -- see
    /// [`ProjectFile::to_project`].
    pub type_id: String,
    /// Every parameter, by id. Ids this build does not know are ignored, which
    /// is rule 1 at parameter granularity -- see
    /// [`ParamBlock::apply`](mydaw_rt_core::ParamBlock::apply).
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
}

impl InsertEntry {
    /// Record a live insert the way a file has to.
    pub fn capture(def: &InsertDef) -> Self {
        Self {
            id: def.id,
            type_id: def.type_id.clone(),
            params: def
                .values()
                .into_iter()
                .map(|(id, value)| (id.to_owned(), value))
                .collect(),
        }
    }

    /// Rebuild the live insert this entry names.
    ///
    /// `None` -- with a warning -- for a `type_id` this build does not have. A
    /// later build's effect is not a corrupt file, and the rest of the chain
    /// still loads; the entry itself is kept in
    /// [`ProjectFile`](crate::ProjectFile) either way, so a save on this machine
    /// does not throw it away.
    pub fn restore(&self, registry: &EffectRegistry) -> Option<InsertDef> {
        match InsertDef::new(self.id, &self.type_id, registry) {
            Ok(def) => {
                def.params
                    .apply(self.params.iter().map(|(id, value)| (id.as_str(), *value)));
                Some(def)
            }
            Err(err) => {
                log::warn!("{err}; leaving it out of the chain");
                None
            }
        }
    }
}

/// One bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusEntry {
    pub id: u32,
    pub name: String,
    /// Absent rather than `null` when there is none -- the engine has no
    /// palette, and inventing one would put a design decision in a file format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub gain_db: f32,
    pub pan: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub soloed: bool,
    #[serde(default)]
    pub inserts: Vec<InsertEntry>,
}

/// One send tap.
///
/// The source is a `(kind, index)` pair rather than a tagged enum, so the file
/// reads the way the command surface talks -- `"track"` / `"bus"` plus a number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendEntry {
    pub id: u32,
    /// `track` or `bus`. Anything else is skipped on load with a warning, on
    /// the same terms as an unknown clip kind.
    pub source_kind: String,
    pub source_index: usize,
    /// Index of the destination bus.
    pub dest: usize,
    /// Linear level. `1.0` is unity.
    pub level: f32,
    #[serde(default)]
    pub pre: bool,
}

impl SendEntry {
    /// Record a live send.
    pub fn capture(send: &SendDef) -> Self {
        Self {
            id: send.id,
            source_kind: if send.source.is_bus() { "bus" } else { "track" }.to_owned(),
            source_index: send.source.index(),
            dest: send.dest,
            level: send.level,
            pre: send.pre,
        }
    }

    /// Rebuild the live send, or `None` with a warning for a source kind this
    /// build does not know.
    pub fn restore(&self) -> Option<SendDef> {
        let source = match self.source_kind.as_str() {
            "track" => StripId::Track(self.source_index),
            "bus" => StripId::Bus(self.source_index),
            other => {
                log::warn!(
                    "send {} comes from a '{other}', which this build does not know; skipping it",
                    self.id
                );
                return None;
            }
        };
        Some(SendDef {
            id: self.id,
            source,
            dest: self.dest,
            level: self.level,
            pre: self.pre,
        })
    }
}

/// What sits at the head of a track.
///
/// Internally tagged on `kind`, so the file reads as
/// `{"kind": "clap", "bundle_path": "...", ...}` -- one object per source rather
/// than a wrapper object around a payload, which is what a human editing the
/// file would expect and what a TypeScript discriminated union reads directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceEntry {
    /// A generator that ships with MYDAW, by the id [`Source::id`] reports.
    Internal { id: String },
    /// A hosted CLAP instrument.
    Clap(ClapEntry),
    /// A hosted VST3 instrument.
    Vst3(Vst3Entry),
}

/// A CLAP plugin, identified the way a reload has to find it again.
///
/// Bundle path **and** plugin id, because neither alone is enough: one bundle
/// commonly exports several plugins, and the same plugin id can be installed in
/// two places.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClapEntry {
    pub bundle_path: String,
    pub plugin_id: String,
    /// Display name from the scan that placed it. Recorded so that a strip for a
    /// **missing** plugin can still be labelled with something a user
    /// recognises, rather than with a path they have to read.
    #[serde(default)]
    pub name: String,
}

/// A VST3 plugin. `cid` is 32 hexadecimal digits, as
/// [`Vst3PluginInfoDto`](crate::dto::Vst3PluginInfoDto) reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vst3Entry {
    pub module_path: String,
    pub cid: String,
    #[serde(default)]
    pub name: String,
}

/// The master insert: which plugin, and its state.
///
/// Its own type rather than a [`TrackEntry`] with the track fields left out,
/// because the master insert has no fader of its own (that is
/// [`ProjectFile::master_gain_db`]) and no name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectEntry {
    pub plugin: EffectRef,
    #[serde(default)]
    pub state_blob: String,
}

/// A master-insert plugin. The same two formats a track source can be, minus
/// `internal` -- there is no built-in master effect to name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectRef {
    Clap(ClapEntry),
    Vst3(Vst3Entry),
}

impl EffectRef {
    /// What to put on the master strip while the plugin is missing.
    pub fn label(&self) -> String {
        match self {
            Self::Clap(entry) => {
                if entry.name.is_empty() {
                    entry.plugin_id.clone()
                } else {
                    entry.name.clone()
                }
            }
            Self::Vst3(entry) => {
                if entry.name.is_empty() {
                    entry.cid.clone()
                } else {
                    entry.name.clone()
                }
            }
        }
    }

    /// Where the binary should be.
    pub fn path(&self) -> &str {
        match self {
            Self::Clap(entry) => &entry.bundle_path,
            Self::Vst3(entry) => &entry.module_path,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a project file could not be read, written or believed.
///
/// Every variant is reachable from a file on disk, which is the least
/// trustworthy input this application has: it can be truncated by a full disk,
/// hand-edited, written by a different build, or not a project file at all.
#[derive(Debug, thiserror::Error)]
pub enum ProjectFileError {
    /// The filesystem said no. Carries the path, because "permission denied"
    /// without one is not an error message.
    #[error("{0}")]
    Io(String),
    /// The bytes are not a project: not JSON, or JSON of the wrong shape.
    #[error("this file is not a MYDAW project ({0})")]
    Malformed(String),
    /// Written by a later MYDAW. Refused rather than misread -- see the module
    /// docs on forward migration.
    #[error(
        "this project needs a newer MYDAW: it is format version {found}, \
         and this build reads version {FORMAT_VERSION}"
    )]
    FutureVersion { found: u32 },
    /// A project with no tracks has no keyboard target, no meters and nothing
    /// to draw.
    #[error("a project must have at least one track")]
    NoTracks,
    #[error("a project may have at most {MAX_TRACKS} tracks; this one has {0}")]
    TooManyTracks(usize),
    #[error("a project may have at most {MAX_BUSES} buses; this one has {0}")]
    TooManyBuses(usize),
    /// One track's data is not usable. Names the track, so the sentence points
    /// at the line to look at.
    #[error("track {track} ('{name}'): {reason}")]
    Track {
        track: usize,
        name: String,
        reason: String,
    },
    #[error("the master effect: {0}")]
    MasterEffect(String),
    #[error("tempo {0} is not a number between {MIN_BPM} and {MAX_BPM}")]
    InvalidBpm(f64),
    #[error("the pattern in this project cannot be run: {0}")]
    Pattern(#[from] PatternError),
    #[error("the arrangement in this project cannot be played: {0}")]
    Playlist(#[from] PlaylistError),
}

// ---------------------------------------------------------------------------
// Capture: session -> file
// ---------------------------------------------------------------------------

impl SourceEntry {
    /// Record a live source the way a file has to.
    pub fn capture(source: &Source) -> Self {
        match source {
            Source::Clap(reference) => Self::Clap(ClapEntry {
                bundle_path: reference.bundle_path.to_string_lossy().into_owned(),
                plugin_id: reference.plugin_id.clone(),
                name: reference.name.clone(),
            }),
            Source::Vst3(reference) => Self::Vst3(Vst3Entry {
                module_path: reference.module_path.to_string_lossy().into_owned(),
                cid: reference.cid.to_hex(),
                name: reference.name.clone(),
            }),
            built_in => Self::Internal { id: built_in.id() },
        }
    }

    /// Whether this entry names a plugin that has to be loaded from disk.
    pub const fn is_hosted(&self) -> bool {
        matches!(self, Self::Clap(_) | Self::Vst3(_))
    }

    /// What to show on a strip whose plugin could not be loaded.
    pub fn label(&self) -> String {
        match self {
            Self::Internal { id } => id.clone(),
            Self::Clap(entry) => {
                if entry.name.is_empty() {
                    entry.plugin_id.clone()
                } else {
                    entry.name.clone()
                }
            }
            Self::Vst3(entry) => {
                if entry.name.is_empty() {
                    entry.cid.clone()
                } else {
                    entry.name.clone()
                }
            }
        }
    }

    /// Rebuild the live source this entry names.
    ///
    /// # Errors
    /// A sentence, for a built-in id this build does not have or a class id that
    /// is not 32 hexadecimal digits. **Not** for a plugin that is merely absent
    /// from the machine: this function never touches the filesystem, and a
    /// missing binary is discovered later, when an instance is created.
    pub fn to_source(&self) -> Result<Source, String> {
        match self {
            Self::Internal { id } => Source::parse(id).map_err(|err| err.to_string()),
            Self::Clap(entry) => Ok(Source::Clap(
                ClapSourceRef::new(&entry.bundle_path, &entry.plugin_id).named(entry.name.clone()),
            )),
            Self::Vst3(entry) => {
                let cid = crate::vst3_host::parse_cid(&entry.cid)?;
                Ok(Source::Vst3(
                    Vst3SourceRef::new(&entry.module_path, cid).named(entry.name.clone()),
                ))
            }
        }
    }
}

impl EffectRef {
    /// Record the plugin on the master insert.
    pub fn clap(
        bundle_path: impl Into<String>,
        plugin_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self::Clap(ClapEntry {
            bundle_path: bundle_path.into(),
            plugin_id: plugin_id.into(),
            name: name.into(),
        })
    }

    /// The VST3 counterpart of [`clap`](Self::clap).
    pub fn vst3(
        module_path: impl Into<String>,
        cid: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self::Vst3(Vst3Entry {
            module_path: module_path.into(),
            cid: cid.into(),
            name: name.into(),
        })
    }

    /// The class id this entry names.
    ///
    /// # Errors
    /// A sentence, for a `cid` that is not 32 hexadecimal digits.
    pub fn class_id(&self) -> Result<ClassId, String> {
        match self {
            Self::Clap(_) => Err("this master effect is a CLAP plugin, not a VST3 one".to_owned()),
            Self::Vst3(entry) => crate::vst3_host::parse_cid(&entry.cid),
        }
    }
}

/// Everything the sequencer contributes to a save, borrowed for the call.
///
/// A struct rather than four positional parameters: at this arity the call site
/// is unreadable, and swapping the set and the playlist would compile.
pub struct Song<'a> {
    pub patterns: &'a PatternSet,
    pub playlist: &'a Playlist,
    pub mode: PlayMode,
    /// Every audio file the arrangement references. Empty until the audio
    /// source registry exists; the field is here because the *format* has to
    /// carry it from version 3 rather than gaining it in version 4.
    pub audio_sources: Vec<AudioSourceEntry>,
}

impl ProjectFile {
    /// Assemble a file from a live session.
    ///
    /// `track_states` is one blob per track, in track order; a short slice is
    /// read as "no state from here on", which is what a session with no hosted
    /// plugins hands in.
    pub fn capture(
        project: &Project,
        selected: usize,
        bpm: f64,
        song: &Song,
        track_states: &[Vec<u8>],
        master_effect: Option<EffectEntry>,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            bpm,
            master_gain_db: project.master_gain_db,
            selected_track: selected.min(project.len().saturating_sub(1)),
            tracks: project
                .tracks
                .iter()
                .enumerate()
                .map(|(index, track)| TrackEntry {
                    id: track.id,
                    name: track.name.clone(),
                    source: SourceEntry::capture(&track.source),
                    gain_db: track.gain_db,
                    pan: track.pan,
                    muted: track.muted,
                    soloed: track.soloed,
                    pitch_cents: track.pitch_cents,
                    sample_start_frames: track.sample_start_frames,
                    sample_end_frames: track.sample_end_frames,
                    state_blob: track_states
                        .get(index)
                        .map_or_else(String::new, |bytes| to_base64(bytes)),
                    inserts: track.inserts.iter().map(InsertEntry::capture).collect(),
                })
                .collect(),
            buses: project
                .buses
                .iter()
                .map(|bus| BusEntry {
                    id: bus.id,
                    name: bus.name.clone(),
                    color: bus.color.clone(),
                    gain_db: bus.gain_db,
                    pan: bus.pan,
                    muted: bus.muted,
                    soloed: bus.soloed,
                    inserts: bus.inserts.iter().map(InsertEntry::capture).collect(),
                })
                .collect(),
            sends: project.sends.iter().map(SendEntry::capture).collect(),
            master_effect,
            master_inserts: project
                .master_inserts
                .iter()
                .map(InsertEntry::capture)
                .collect(),
            play_mode: song.mode.id().to_owned(),
            patterns: song
                .patterns
                .patterns()
                .iter()
                .map(PatternDto::from)
                .collect(),
            active_pattern: song.patterns.active_id(),
            playlist: PlaylistEntry {
                tracks: song
                    .playlist
                    .tracks()
                    .iter()
                    .map(|track| PlaylistTrackEntry {
                        id: track.id,
                        name: track.name.clone(),
                        muted: track.muted,
                    })
                    .collect(),
                clips: song
                    .playlist
                    .clips()
                    .iter()
                    .map(ClipEntry::capture)
                    .collect(),
            },
            audio_sources: song.audio_sources.clone(),
            pattern: None,
        }
    }

    /// Refuse anything that would not survive the trip to JSON and back.
    ///
    /// Only the floats can do that: `serde_json` writes a non-finite number as
    /// `null`, and `null` will not deserialise back into an `f32`. So a NaN
    /// gain would produce a file that this build itself could not open, which is
    /// the one failure mode a save must never have. Everything here is already
    /// guaranteed by the validation on the way *in* to the session; this is the
    /// belt to that pair of braces, and it is cheap.
    ///
    /// # Errors
    /// [`ProjectFileError::Track`], [`ProjectFileError::InvalidBpm`].
    pub fn validate(&self) -> Result<(), ProjectFileError> {
        if !self.bpm.is_finite() || !(MIN_BPM..=MAX_BPM).contains(&self.bpm) {
            return Err(ProjectFileError::InvalidBpm(self.bpm));
        }
        if !self.master_gain_db.is_finite() {
            return Err(ProjectFileError::Track {
                track: 0,
                name: "master".to_owned(),
                reason: format!("the master fader is {}", self.master_gain_db),
            });
        }
        for (index, track) in self.tracks.iter().enumerate() {
            let bad = |reason: String| ProjectFileError::Track {
                track: index,
                name: track.name.clone(),
                reason,
            };
            check_gain_db(track.gain_db).map_err(|err| bad(err.to_string()))?;
            check_pan(track.pan).map_err(|err| bad(err.to_string()))?;
        }
        Ok(())
    }

    /// The mixer this file describes, and the track the session should select.
    ///
    /// A track headed by a hosted plugin comes back as [`Source::Clap`] or
    /// [`Source::Vst3`] **whether or not the plugin is installed**: the graph
    /// renders such a track as silence until a host attaches an instance to it,
    /// which is exactly the state a load passes through anyway.
    ///
    /// # Errors
    /// [`ProjectFileError::NoTracks`], [`ProjectFileError::TooManyTracks`],
    /// [`ProjectFileError::Track`] for a value the mixer would refuse or a
    /// source this build cannot name.
    pub fn to_project(
        &self,
        effects: &EffectRegistry,
    ) -> Result<(Project, usize), ProjectFileError> {
        if self.tracks.is_empty() {
            return Err(ProjectFileError::NoTracks);
        }
        if self.tracks.len() > MAX_TRACKS {
            return Err(ProjectFileError::TooManyTracks(self.tracks.len()));
        }
        if self.buses.len() > MAX_BUSES {
            return Err(ProjectFileError::TooManyBuses(self.buses.len()));
        }

        let mut defs = Vec::with_capacity(self.tracks.len());
        for (index, entry) in self.tracks.iter().enumerate() {
            let bad = |reason: String| ProjectFileError::Track {
                track: index,
                name: entry.name.clone(),
                reason,
            };
            let source = entry.source.to_source().map_err(&bad)?;
            let gain_db = check_gain_db(entry.gain_db).map_err(|err| bad(err.to_string()))?;
            let pan = check_pan(entry.pan).map_err(|err| bad(err.to_string()))?;
            // Rejected rather than skipped: a blob that is not base64 means the
            // file has been damaged, and loading the track without its state
            // would silently give the user a plugin at its defaults.
            let _ = from_base64(&entry.state_blob).map_err(&bad)?;
            defs.push(TrackDef {
                id: entry.id,
                name: entry.name.clone(),
                source,
                gain_db,
                pan,
                muted: entry.muted,
                soloed: entry.soloed,
                pitch_cents: entry.pitch_cents,
                sample_start_frames: entry.sample_start_frames,
                sample_end_frames: entry.sample_end_frames,
                inserts: restore_chain(&entry.inserts, effects),
            });
        }

        let buses: Vec<BusDef> = self
            .buses
            .iter()
            .map(|entry| BusDef {
                id: entry.id,
                name: if entry.name.trim().is_empty() {
                    format!("Bus {}", entry.id + 1)
                } else {
                    entry.name.clone()
                },
                color: entry.color.clone(),
                // Clamped rather than refused: a bus is not a track, its fader
                // is not something the file's own validation ever checked, and a
                // project that will not open over a stray decibel is worse than
                // one that opens at the edge of the scale.
                gain_db: entry.gain_db.clamp(SILENCE_DB, MAX_GAIN_DB),
                pan: if entry.pan.is_finite() {
                    entry.pan.clamp(-1.0, 1.0)
                } else {
                    0.0
                },
                muted: entry.muted,
                soloed: entry.soloed,
                inserts: restore_chain(&entry.inserts, effects),
            })
            .collect();

        // A send naming a strip this project does not have is dropped with a
        // warning rather than refusing the file: it can only come from a file
        // that was hand-edited or written by a build that allowed more strips,
        // and the rest of the routing still works.
        let mut sends: Vec<SendDef> = Vec::new();
        for entry in &self.sends {
            let Some(send) = entry.restore() else {
                continue;
            };
            let source_exists = match send.source {
                StripId::Track(index) => index < defs.len(),
                StripId::Bus(index) => index < buses.len(),
            };
            if !source_exists || send.dest >= buses.len() || send.source == StripId::Bus(send.dest)
            {
                log::warn!(
                    "send {} runs {} -> bus {}, which this project does not have; dropping it",
                    send.id,
                    send.source,
                    send.dest
                );
                continue;
            }
            if sends
                .iter()
                .any(|kept| kept.source == send.source && kept.dest == send.dest)
            {
                log::warn!("send {} duplicates an earlier one; dropping it", send.id);
                continue;
            }
            sends.push(SendDef {
                level: crate::project::check_send_level(send.level).unwrap_or(1.0),
                ..send
            });
        }
        drop_send_cycles(&mut sends);

        if let Some(effect) = &self.master_effect {
            from_base64(&effect.state_blob).map_err(ProjectFileError::MasterEffect)?;
            if let EffectRef::Vst3(_) = &effect.plugin {
                effect
                    .plugin
                    .class_id()
                    .map_err(ProjectFileError::MasterEffect)?;
            }
        }

        let selected = self.selected_track.min(defs.len() - 1);
        Ok((
            Project::restored_with(
                defs,
                self.master_gain_db,
                buses,
                sends,
                restore_chain(&self.master_inserts, effects),
            ),
            selected,
        ))
    }

    /// The step grid this file describes.
    ///
    /// # Errors
    /// [`ProjectFileError::Pattern`] for a grid the engine could not run -- a
    /// row of the wrong length, a key outside the MIDI range, a velocity that is
    /// not a number.
    pub fn to_song(&self) -> Result<(PatternSet, Playlist), ProjectFileError> {
        let mut patterns = Vec::with_capacity(self.patterns.len());
        for dto in &self.patterns {
            patterns.push(Pattern::try_from(dto)?);
        }
        let active = patterns
            .iter()
            .position(|pattern| pattern.id == self.active_pattern)
            .unwrap_or(0);
        let set = PatternSet::restored(patterns, active);

        let tracks: Vec<PlaylistTrack> = self
            .playlist
            .tracks
            .iter()
            .map(|track| PlaylistTrack {
                id: track.id,
                name: if track.name.trim().is_empty() {
                    format!("Track {}", track.id)
                } else {
                    track.name.clone()
                },
                muted: track.muted,
            })
            .collect();
        let clips: Vec<Clip> = self
            .playlist
            .clips
            .iter()
            .filter_map(ClipEntry::restore)
            .collect();
        let playlist = if tracks.is_empty() && clips.is_empty() {
            Playlist::default()
        } else {
            Playlist::restored(tracks, clips)
        };
        playlist.validate(&set)?;
        Ok((set, playlist))
    }

    /// Which play mode the file was saved in. Anything unrecognised is pattern
    /// mode, which is the answer that cannot surprise anyone.
    pub fn to_play_mode(&self) -> PlayMode {
        PlayMode::parse(&self.play_mode).unwrap_or_default()
    }

    /// The tempo this file describes, checked against the transport's range.
    ///
    /// # Errors
    /// [`ProjectFileError::InvalidBpm`].
    pub fn to_bpm(&self) -> Result<f64, ProjectFileError> {
        if !self.bpm.is_finite() || !(MIN_BPM..=MAX_BPM).contains(&self.bpm) {
            return Err(ProjectFileError::InvalidBpm(self.bpm));
        }
        Ok(self.bpm)
    }

    /// One track's plugin state, decoded.
    ///
    /// # Errors
    /// A sentence, for base64 that is not base64. Unreachable after
    /// [`to_project`](Self::to_project) has accepted the file, which decodes
    /// every blob for exactly this reason.
    pub fn track_state(&self, track: usize) -> Result<Vec<u8>, String> {
        match self.tracks.get(track) {
            Some(entry) => from_base64(&entry.state_blob),
            None => Ok(Vec::new()),
        }
    }

    /// The master effect's state, decoded. Empty when there is no effect.
    ///
    /// # Errors
    /// See [`track_state`](Self::track_state).
    pub fn master_state(&self) -> Result<Vec<u8>, String> {
        match &self.master_effect {
            Some(effect) => from_base64(&effect.state_blob),
            None => Ok(Vec::new()),
        }
    }

    /// The JSON this file serialises to, exactly as it is written to disk.
    ///
    /// Public so a test can assert byte-identity without going near a
    /// filesystem, and so a save can be checked before anything is replaced.
    ///
    /// # Errors
    /// [`ProjectFileError::Track`] / [`ProjectFileError::InvalidBpm`] from
    /// [`validate`](Self::validate); serialisation itself cannot fail for a
    /// validated project.
    pub fn to_json(&self) -> Result<String, ProjectFileError> {
        self.validate()?;
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|err| ProjectFileError::Malformed(err.to_string()))?;
        // A trailing newline: this is a text file, and every tool that touches
        // one expects the last line to be terminated.
        json.push('\n');
        Ok(json)
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Rebuild one insert chain, leaving out any effect this build does not have.
fn restore_chain(entries: &[InsertEntry], effects: &EffectRegistry) -> Vec<InsertDef> {
    entries
        .iter()
        .filter_map(|entry| entry.restore(effects))
        .take(crate::effects::MAX_INSERTS)
        .collect()
}

/// Drop the bus-to-bus taps that would close a loop.
///
/// A cycle cannot be created through the command surface -- [`Project::add_send`]
/// refuses one -- so this only ever fires on a hand-edited file. Dropping the
/// edge that closes the loop is the same answer the graph compiler gives, and it
/// keeps a damaged file openable instead of turning a routing typo into "your
/// project will not load".
///
/// [`Project::add_send`]: crate::Project::add_send
fn drop_send_cycles(sends: &mut Vec<SendDef>) {
    let mut kept: Vec<SendDef> = Vec::with_capacity(sends.len());
    for send in sends.drain(..) {
        let StripId::Bus(from) = send.source else {
            kept.push(send);
            continue;
        };
        // Would `dest` already reach `from`? If so this edge closes a loop.
        let mut frontier = vec![send.dest];
        let mut seen = vec![send.dest];
        let mut closes = from == send.dest;
        while let Some(bus) = frontier.pop() {
            if closes {
                break;
            }
            for edge in &kept {
                if edge.source != StripId::Bus(bus) {
                    continue;
                }
                if edge.dest == from {
                    closes = true;
                    break;
                }
                if !seen.contains(&edge.dest) {
                    seen.push(edge.dest);
                    frontier.push(edge.dest);
                }
            }
        }
        if closes {
            log::warn!(
                "send {} would feed bus {from} into itself; dropping it",
                send.id
            );
        } else {
            kept.push(send);
        }
    }
    *sends = kept;
}

/// Just enough of a project file to decide whether to read the rest.
#[derive(Deserialize)]
struct VersionProbe {
    format_version: Option<u32>,
}

/// Parse a project from text, migrating an older format on the way.
///
/// The version is read first, from a probe that knows about one field, so that a
/// file from a future MYDAW is refused with [`ProjectFileError::FutureVersion`]
/// rather than with whatever type error its new shape happens to produce.
///
/// An **older** file is upgraded rather than refused -- see the module docs on
/// forward migration. The returned [`ProjectFile`] always carries
/// [`FORMAT_VERSION`], whatever the bytes said, so that a load-then-save writes
/// the current format and the byte-identity property holds from the second save
/// onwards rather than never.
///
/// # Errors
/// [`ProjectFileError::Malformed`] for anything that is not a project (bad
/// JSON, truncation, a missing `format_version`, a field of the wrong type), or
/// [`ProjectFileError::FutureVersion`].
pub fn parse_project(text: &str) -> Result<ProjectFile, ProjectFileError> {
    let probe: VersionProbe =
        serde_json::from_str(text).map_err(|err| ProjectFileError::Malformed(err.to_string()))?;
    let Some(found) = probe.format_version else {
        return Err(ProjectFileError::Malformed(
            "it has no `format_version` field".to_owned(),
        ));
    };
    if found > FORMAT_VERSION {
        return Err(ProjectFileError::FutureVersion { found });
    }
    let mut file: ProjectFile =
        serde_json::from_str(text).map_err(|err| ProjectFileError::Malformed(err.to_string()))?;

    // Version 1 and 2 carried one unnamed pattern and no arrangement. Both
    // become a version 3 file with one pattern called "Pattern 1" and an empty
    // playlist -- which is exactly what those files meant, because there was
    // nowhere else for a pattern to be placed.
    if file.patterns.is_empty() {
        if let Some(legacy) = file.pattern.take() {
            file.patterns = vec![migrate_single_pattern(&legacy)];
        }
    }
    file.pattern = None;
    if file.active_pattern == 0 {
        file.active_pattern = file.patterns.first().map_or(1, |pattern| pattern.id);
    }
    if file.play_mode.trim().is_empty() {
        file.play_mode = default_play_mode();
    }
    file.format_version = FORMAT_VERSION;
    Ok(file)
}

/// Read and parse a project file.
///
/// # Errors
/// [`ProjectFileError::Io`] naming the path, plus everything
/// [`parse_project`] reports.
pub fn read_project_file(path: &Path) -> Result<ProjectFile, ProjectFileError> {
    let text = fs::read_to_string(path)
        .map_err(|err| ProjectFileError::Io(format!("could not read {}: {err}", path.display())))?;
    parse_project(&text)
}

// ---------------------------------------------------------------------------
// Atomic write
// ---------------------------------------------------------------------------

/// The sibling file a save is written through before it is renamed into place.
///
/// A save must never be able to destroy the project it is replacing. Writing in
/// place cannot promise that: a full disk, a crash or a pulled cable halfway
/// through leaves a truncated file where a session used to be. So the bytes go
/// to a sibling first, are flushed all the way to the device, and only then
/// replace the original with a single rename -- which is atomic on both Windows
/// (`MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`, which is what `fs::rename`
/// compiles to) and POSIX. Either the old file is there or the new one is;
/// there is no moment when neither is.
///
/// A *sibling* rather than the system temp directory, because a rename is only
/// atomic within one volume and `%TEMP%` is routinely on another.
///
/// The name is deterministic (process id included, so two MYDAWs saving the same
/// path cannot collide) so that a test can occupy it and make the write fail on
/// purpose.
pub fn temp_path_for(path: &Path) -> PathBuf {
    let name = path.file_name().map_or_else(
        || PROJECT_EXTENSION.to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let temp = format!(".{name}.{}.tmp", std::process::id());
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(temp),
        _ => PathBuf::from(temp),
    }
}

/// Write `file` to `path`, atomically.
///
/// See [`temp_path_for`] for what "atomically" buys and how it is done. On any
/// failure the temporary file is removed and **whatever was at `path` is
/// untouched**.
///
/// # Errors
/// [`ProjectFileError::Io`] naming the path, or the validation errors
/// [`ProjectFile::to_json`] reports.
pub fn write_project_file(path: &Path, file: &ProjectFile) -> Result<(), ProjectFileError> {
    let json = file.to_json()?;
    let temp = temp_path_for(path);

    let outcome = (|| -> std::io::Result<()> {
        let mut handle = fs::File::create(&temp)?;
        handle.write_all(json.as_bytes())?;
        // All the way to the device before the rename: a rename that beats its
        // own data to disk is exactly the corruption this dance exists to
        // prevent.
        handle.sync_all()?;
        drop(handle);
        fs::rename(&temp, path)
    })();

    if let Err(err) = outcome {
        // Best effort: the write already failed, and failing to clean up after
        // it is not a second error worth reporting over the first.
        let _ = fs::remove_file(&temp);
        return Err(ProjectFileError::Io(format!(
            "could not save {}: {err}",
            path.display()
        )));
    }
    Ok(())
}

/// The name a window title shows for a path: the file name without `.mydaw`.
pub fn project_name(path: Option<&Path>) -> String {
    path.and_then(Path::file_stem).map_or_else(
        || UNTITLED.to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::InsertTarget;
    use mydaw_plugin_sequencer::{MelodicNote, Step, StepRow, TICKS_PER_STEP};
    use mydaw_rt_core::{MAX_GAIN_DB, SILENCE_DB};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch directory that deletes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mydaw-project-file-{tag}-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("scratch dir");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn clap_source() -> Source {
        Source::Clap(
            ClapSourceRef::new(r"C:\CLAP\Fixture.clap", "dev.mydaw.fixture.synth")
                .named("MYDAW Fixture Synth"),
        )
    }

    /// Two bars of drums with a melody over them: everything the pattern half
    /// of the format has to carry, including a length that is not the default
    /// and a note that is not on the grid.
    fn beat() -> Pattern {
        let mut pattern = Pattern::with_channels(1, "Pattern 1", &[0, 1, 2, 3], |channel| {
            mydaw_plugin_sequencer::DEFAULT_ROW_KEYS[channel as usize]
        });
        pattern.length_steps = 32;
        pattern.color = Some("#4f8ef7".to_owned());
        pattern.channels[0].steps =
            Some(StepRow::empty(36).with_steps(&[0, 4, 8, 12, 16, 20, 24, 28]));
        pattern.channels[2].steps = Some(StepRow::empty(42).with_steps(&[2, 6, 10, 14]));
        pattern.channels[1].steps.as_mut().unwrap().steps[4] = Step::accent();
        pattern.channels[1].notes = vec![
            MelodicNote::new(60, 0, 960),
            MelodicNote::new(64, 960, 480).at_velocity(0.31),
        ];
        // Deliberately between the grid lines: a triplet eighth, which is the
        // thing a step index could not have expressed.
        pattern.channels[2].notes = vec![MelodicNote::new(67, 1_920 + 320, 320)];
        pattern
    }

    /// A second pattern and an arrangement of both: the version 3 half of the
    /// format, and the half a version 2 file could not have said anything about.
    fn song() -> (PatternSet, Playlist) {
        let mut set = PatternSet::single(beat());
        let second = set.add(Some("Bridge".to_owned()), &[0, 1], |_| 36).unwrap();
        set.get_mut(second).unwrap().channels[0].steps =
            Some(StepRow::empty(36).with_steps(&[0, 8]));
        set.set_active(second).unwrap();

        let mut playlist = Playlist::default();
        playlist.rename_track(0, "Drums").unwrap();
        playlist.set_track_muted(3, true).unwrap();
        playlist.add_pattern_clip(1, 0, 0, 2 * 3_840).unwrap();
        playlist
            .add_pattern_clip(1, 0, 4 * 3_840, 2 * 3_840)
            .unwrap();
        playlist
            .add_pattern_clip(second, 1, 4 * 3_840, 4 * 3_840)
            .unwrap();
        (set, playlist)
    }

    /// The project the round-trip tests use: three tracks, one of them hosted,
    /// every mixer control off its default.
    fn session() -> (Project, PatternSet, Playlist) {
        let mut project = Project::default();
        project
            .add(Source::TestTone, Some("Tone".to_owned()))
            .expect("room");
        project.tracks[0].gain_db = -6.5;
        project.tracks[0].pan = -0.25;
        project.tracks[0].muted = true;
        project.tracks[1].source = clap_source();
        project.tracks[1].name = "Fixture".to_owned();
        project.tracks[1].gain_db = 3.0;
        project.tracks[1].soloed = true;
        project.tracks[2].pan = 1.0;
        project.master_gain_db = -3.5;
        let (set, playlist) = song();
        (project, set, playlist)
    }

    /// The song a project with nothing written in it has: one empty pattern
    /// and an empty arrangement.
    fn empty_song<'a>() -> Song<'a> {
        Song {
            patterns: Box::leak(Box::new(PatternSet::default())),
            playlist: Box::leak(Box::new(Playlist::default())),
            mode: PlayMode::Pattern,
            audio_sources: Vec::new(),
        }
    }

    fn captured() -> ProjectFile {
        let (project, patterns, playlist) = session();
        ProjectFile::capture(
            &project,
            1,
            147.0,
            &Song {
                patterns: &patterns,
                playlist: &playlist,
                mode: PlayMode::Pattern,
                audio_sources: Vec::new(),
            },
            &[Vec::new(), b"MFS1\x00\x00\x40\x41".to_vec(), Vec::new()],
            Some(EffectEntry {
                plugin: EffectRef::clap(r"C:\CLAP\Fixture.clap", "dev.mydaw.fixture.gain", "Gain"),
                state_blob: String::new(),
            }),
        )
    }

    #[test]
    fn the_version_is_the_first_field_in_the_file() {
        // A human opening the file, and a future MYDAW reading only its head,
        // both see the version before anything they might misinterpret.
        let json = captured().to_json().unwrap();
        let first = json
            .lines()
            .nth(1)
            .expect("a pretty-printed object has a second line");
        assert_eq!(
            first.trim(),
            format!(r#""format_version": {FORMAT_VERSION},"#),
            "{json}"
        );
        assert_eq!(FORMAT_VERSION, 4, "buses, sends and insert chains");
        assert!(json.ends_with("}\n"), "the file ends with a newline");
    }

    #[test]
    fn a_session_round_trips_through_the_file_with_every_field_intact() {
        let (project, patterns, playlist) = session();
        let file = captured();
        let text = file.to_json().unwrap();
        let parsed = parse_project(&text).expect("what we wrote is what we read");
        assert_eq!(parsed, file);

        let (restored, selected) = parsed
            .to_project(&EffectRegistry::new())
            .expect("a valid project");
        assert_eq!(restored, project, "the whole mixer, field for field");
        assert_eq!(selected, 1);
        assert_eq!(parsed.to_bpm().unwrap(), 147.0);
        let (restored_patterns, restored_playlist) = parsed.to_song().expect("a valid song");
        assert_eq!(restored_patterns, patterns, "every pattern, note for note");
        assert_eq!(restored_playlist, playlist, "and the whole arrangement");
        assert_eq!(parsed.to_play_mode(), PlayMode::Pattern);
        assert_eq!(parsed.track_state(1).unwrap(), b"MFS1\x00\x00\x40\x41");
        assert!(parsed.track_state(0).unwrap().is_empty());
        assert_eq!(
            parsed.master_effect.as_ref().unwrap().plugin.label(),
            "Gain"
        );
    }

    #[test]
    fn re_saving_a_loaded_project_produces_byte_identical_json() {
        // The property the whole format rests on: the field order is fixed, no
        // value is re-derived on the way through, and every float round trips
        // exactly. Without it a project file could not be diffed, and "nothing
        // was lost" would have to be asserted field by field for ever.
        let first = captured().to_json().unwrap();
        let second = parse_project(&first).unwrap().to_json().unwrap();
        assert_eq!(first, second);
        let third = parse_project(&second).unwrap().to_json().unwrap();
        assert_eq!(
            second, third,
            "and it is stable, not merely idempotent once"
        );
    }

    #[test]
    fn awkward_floats_survive_the_trip_exactly() {
        // A fader is dragged to arbitrary values; a tempo is typed. Both have to
        // come back as the same bits, or a reload would silently change the mix.
        let mut project = Project::default();
        project.tracks[0].gain_db = -6.020_6;
        project.tracks[0].pan = 0.333_333_34;
        project.tracks[1].gain_db = SILENCE_DB;
        project.master_gain_db = MAX_GAIN_DB;
        let file = ProjectFile::capture(&project, 0, 174.3, &empty_song(), &[], None);

        let parsed = parse_project(&file.to_json().unwrap()).unwrap();
        let (restored, _) = parsed.to_project(&EffectRegistry::new()).unwrap();
        assert_eq!(restored.tracks[0].gain_db, -6.020_6);
        assert_eq!(restored.tracks[0].pan, 0.333_333_34);
        assert_eq!(restored.tracks[1].gain_db, SILENCE_DB);
        assert_eq!(restored.master_gain_db, MAX_GAIN_DB);
        assert_eq!(parsed.to_bpm().unwrap(), 174.3);
    }

    #[test]
    fn a_hosted_source_records_both_halves_of_its_locator() {
        // Bundle path *and* plugin id: one bundle exports several plugins, and
        // the same id can be installed twice. Either alone would reload the
        // wrong thing, silently.
        let entry = SourceEntry::capture(&clap_source());
        let SourceEntry::Clap(clap) = &entry else {
            panic!("a CLAP source records itself as one: {entry:?}");
        };
        assert_eq!(clap.plugin_id, "dev.mydaw.fixture.synth");
        assert_eq!(clap.bundle_path, r"C:\CLAP\Fixture.clap");
        assert_eq!(clap.name, "MYDAW Fixture Synth");
        assert!(entry.is_hosted());
        assert_eq!(entry.label(), "MYDAW Fixture Synth");
        assert_eq!(entry.to_source().unwrap(), clap_source());

        let vst3 = Source::Vst3(
            Vst3SourceRef::new(
                r"C:\VST3\Thing.vst3",
                ClassId::from_hex("0123456789ABCDEF0123456789ABCDEF").unwrap(),
            )
            .named("Thing"),
        );
        let entry = SourceEntry::capture(&vst3);
        assert_eq!(entry.to_source().unwrap(), vst3);
        let json = serde_json::to_string(&entry).unwrap();
        assert!(
            json.starts_with(r#"{"kind":"vst3","module_path""#),
            "{json}"
        );
    }

    #[test]
    fn a_built_in_source_records_the_id_the_picker_uses() {
        for source in Source::BUILT_IN {
            let entry = SourceEntry::capture(&source);
            assert_eq!(entry, SourceEntry::Internal { id: source.id() });
            assert!(!entry.is_hosted());
            assert_eq!(entry.to_source().unwrap(), source);
        }
        let json = serde_json::to_string(&SourceEntry::capture(&Source::DrumKit)).unwrap();
        assert_eq!(json, r#"{"kind":"internal","id":"drum-kit"}"#);
    }

    /// A sampled instrument persists for free, and that is the point: its id is
    /// complete, so it needs no `SourceEntry` variant, no path in the file, and
    /// no loader on the way back in.
    #[test]
    fn a_sampled_instrument_round_trips_as_an_internal_source() {
        let source = Source::Sampler(
            crate::source::SamplerSourceRef::new("salamander-piano")
                .named("Salamander Grand Piano"),
        );
        let entry = SourceEntry::capture(&source);

        assert_eq!(
            entry,
            SourceEntry::Internal {
                id: "sampler:salamander-piano".to_owned()
            },
            "a bundled instrument is internal, not hosted"
        );
        assert!(
            !entry.is_hosted(),
            "nothing has to be loaded from disk by a plugin host"
        );

        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"internal","id":"sampler:salamander-piano"}"#
        );

        // Back out again. The display name is *not* restored -- it belongs to
        // the registry, not to the file -- so identity is compared by id.
        let restored = entry.to_source().unwrap();
        assert_eq!(restored.id(), source.id());
        assert_eq!(
            restored.sampler_ref().map(|r| r.instrument_id.as_str()),
            Some("salamander-piano")
        );
        assert_eq!(entry.label(), "sampler:salamander-piano");

        // And a project file carrying one parses on a machine with no assets:
        // the source survives, and the track is silent until they arrive.
        let parsed: SourceEntry =
            serde_json::from_str(r#"{"kind":"internal","id":"sampler:not-installed"}"#).unwrap();
        assert_eq!(parsed.to_source().unwrap().id(), "sampler:not-installed");
    }

    #[test]
    fn a_windows_path_survives_json_with_its_backslashes() {
        // The one encoding trap in a Windows-first DAW: a path is escaped going
        // in and must be exactly itself coming out.
        let file = captured();
        let text = file.to_json().unwrap();
        assert!(
            text.contains(r"C:\\CLAP\\Fixture.clap"),
            "escaped in the file"
        );
        let parsed = parse_project(&text).unwrap();
        let (project, _) = parsed.to_project(&EffectRegistry::new()).unwrap();
        assert_eq!(
            project.tracks[1].source.clap_ref().unwrap().bundle_path,
            std::path::PathBuf::from(r"C:\CLAP\Fixture.clap")
        );
    }

    // ------------------------------------------------------------- rejections

    #[test]
    fn a_file_from_a_newer_mydaw_is_refused_by_name_rather_than_misread() {
        let mut file = captured();
        file.format_version = FORMAT_VERSION + 1;
        let text = serde_json::to_string_pretty(&file).unwrap();
        let err = parse_project(&text).unwrap_err();
        assert!(
            matches!(err, ProjectFileError::FutureVersion { found } if found == FORMAT_VERSION + 1),
            "{err:?}"
        );
        assert!(err.to_string().contains("newer MYDAW"), "{err}");

        // ...and the gate fires even when the rest of the file is unreadable to
        // this build, which is the whole point of probing the version first.
        let text = r#"{"format_version": 99, "tracks": "who knows"}"#;
        assert!(matches!(
            parse_project(text),
            Err(ProjectFileError::FutureVersion { found: 99 })
        ));
    }

    #[test]
    fn unknown_fields_are_ignored_so_a_later_format_still_opens() {
        // Rule one of forward migration: a file that gained a field this build
        // does not know about still loads, minus that field.
        let text = captured().to_json().unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["swing"] = serde_json::json!(0.56);
        value["tracks"][0]["colour"] = serde_json::json!("#ff8800");
        value["tracks"][0]["source"]["latency_hint"] = serde_json::json!(128);
        let parsed = parse_project(&value.to_string()).expect("unknown fields are tolerated");
        assert_eq!(parsed.tracks[0].name, "Kick");
        assert_eq!(parsed, captured());
    }

    // ------------------------------------------------------------- migration

    /// A real version 1 project, written by the step 8 build and kept verbatim.
    ///
    /// Checked in rather than generated: the point of a migration test is to
    /// read the *format*, and a fixture this build produced would only prove
    /// that this build agrees with itself.
    const V1_PROJECT: &str = include_str!("../tests/fixtures/v1.mydaw");

    #[test]
    fn a_version_one_project_opens_with_an_empty_piano_roll() {
        // The fixture is genuinely version 1 -- if somebody "helpfully" updates
        // it, this test stops testing anything.
        assert!(
            V1_PROJECT.contains(r#""format_version": 1"#),
            "the fixture must stay a version 1 file"
        );
        assert!(
            !V1_PROJECT.contains("\"notes\""),
            "a version 1 file has no piano roll at all"
        );

        let parsed = parse_project(V1_PROJECT).expect("a version 1 project still opens");

        // Everything version 1 could say is still said.
        assert_eq!(parsed.to_bpm().unwrap(), 147.0);
        assert_eq!(parsed.master_gain_db, -3.5);
        let (project, selected) = parsed
            .to_project(&EffectRegistry::new())
            .expect("a valid project");
        assert_eq!(project.len(), 2);
        assert_eq!(project.tracks[0].name, "Drums");
        assert_eq!(project.tracks[0].gain_db, -6.5);
        assert!(project.tracks[1].soloed);
        assert_eq!(
            project.tracks[1].source.clap_ref().unwrap().plugin_id,
            "dev.mydaw.fixture.synth"
        );
        assert_eq!(selected, 1);
        assert_eq!(parsed.track_state(1).unwrap(), b"MFS1\x00\x00\x40\x41");
        assert_eq!(
            parsed.master_effect.as_ref().unwrap().plugin.label(),
            "Gain"
        );

        // And the new halves are empty rather than absent, which is what a
        // project written before the piano roll and the playlist existed meant.
        let (set, playlist) = parsed.to_song().expect("the grid still runs");
        assert_eq!(set.len(), 1, "version 1 had exactly one pattern");
        let pattern = set.active();
        assert_eq!(pattern.name, "Pattern 1");
        assert_eq!(pattern.length_steps, 16, "version 1 was always one bar");
        // Version 1's four drum rows were three on channel 0 and one on
        // channel 1 -- a drum-kit track, where the *key* picked the drum. A
        // channel has one grid now, so the kick keeps it and the hats become the
        // notes they always meant: fourteen events either way.
        assert_eq!(
            pattern
                .channels
                .iter()
                .map(|c| c.channel)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(pattern.hit_count(), 4 + 2, "the two grids that survived");
        assert_eq!(pattern.note_count(), 8, "the eight hats, as notes");
        assert_eq!(
            pattern.compile().note_count(),
            14,
            "four kicks, two snares, eight hats -- the same events as version 1"
        );
        assert_eq!(
            pattern.channel(1).unwrap().steps.as_ref().unwrap().steps[4].velocity,
            1.0,
            "the accent survived"
        );
        assert!(playlist.clips().is_empty(), "there was nowhere to place it");
        assert_eq!(parsed.to_play_mode(), PlayMode::Pattern);
    }

    #[test]
    fn opening_a_version_one_project_and_saving_it_writes_the_current_version() {
        // The migration policy in one test: read old, write current. The user
        // never sees a dialog and never loses the file -- it is simply a
        // version 2 project the next time it is opened.
        let migrated = parse_project(V1_PROJECT).unwrap();
        assert_eq!(
            migrated.format_version, FORMAT_VERSION,
            "the parsed file carries the current version, whatever the bytes said"
        );

        let saved = migrated.to_json().unwrap();
        assert!(
            saved.contains(r#""format_version": 4"#),
            "a save writes the current format"
        );
        assert!(
            saved.contains(r#""playlist""#) && saved.contains(r#""patterns""#),
            "and the new fields are written explicitly rather than omitted"
        );
        assert!(
            !saved.contains(r#""pattern":"#),
            "the version 2 field is gone, not left vestigial: {saved}"
        );

        // From there the ordinary byte-identity property holds.
        let reopened = parse_project(&saved).unwrap();
        assert_eq!(reopened, migrated);
        assert_eq!(reopened.to_json().unwrap(), saved);
        assert_eq!(reopened.to_song().unwrap(), migrated.to_song().unwrap());
    }

    /// A version 3 project with audio clips and a source table.
    ///
    /// Committed rather than generated, for the reason every fixture here is:
    /// the point of the test is to read the **format**, and a file this build
    /// wrote would only prove that this build agrees with itself. One of its two
    /// sources names a file that will never exist on any machine, because
    /// "the samples folder moved" is the case that has to be a warning rather
    /// than a refusal.
    const V3_AUDIO_PROJECT: &str = include_str!("../tests/fixtures/v3-audio.mydaw");

    #[test]
    fn a_version_three_project_restores_its_audio_clips_and_its_source_table() {
        assert!(
            V3_AUDIO_PROJECT.contains(r#""format_version": 3"#),
            "the fixture must stay a version 3 file"
        );

        let parsed = parse_project(V3_AUDIO_PROJECT).expect("a version 3 project opens");
        assert_eq!(parsed.format_version, FORMAT_VERSION);
        assert_eq!(parsed.to_bpm().unwrap(), 130.0);
        assert_eq!(parsed.to_play_mode(), PlayMode::Song);

        // The source table, in file order.
        assert_eq!(
            parsed
                .audio_sources
                .iter()
                .map(|entry| (entry.id, entry.path.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (1, r"C:\takes\vox-take-4.wav"),
                (2, r"C:\takes\gone-missing.flac"),
            ]
        );

        let (_set, playlist) = parsed.to_song().expect("the song runs");
        assert_eq!(playlist.tracks().len(), 2);
        assert_eq!(playlist.clips().len(), 3);
        assert_eq!(playlist.end_ticks(), 23_040 + 3_840);

        // The audio clips kept every field that only they have. `offset_frames`
        // in particular is in the *file's* frames and must not have been
        // converted to anything.
        let audio: Vec<_> = playlist
            .clips()
            .iter()
            .filter_map(|clip| match clip {
                Clip::Audio(audio) => Some(*audio),
                Clip::Pattern(_) => None,
            })
            .collect();
        assert_eq!(audio.len(), 2);
        assert_eq!(audio[0].id, 2);
        assert_eq!(audio[0].source_id, 1);
        assert_eq!(audio[0].track, 1);
        assert_eq!(audio[0].start_ticks, 11_520, "bar 3 at 3840 ticks a bar");
        assert_eq!(audio[0].length_ticks, 7_680);
        assert_eq!(audio[0].offset_frames, 22_050);
        assert_eq!(audio[0].gain_db, -3.5);
        assert_eq!(audio[1].source_id, 2);
        assert_eq!(audio[1].offset_frames, 0);
    }

    #[test]
    fn a_version_three_project_with_audio_survives_a_save_and_reopen_byte_for_byte() {
        let parsed = parse_project(V3_AUDIO_PROJECT).expect("opens");
        let saved = parsed.to_json().expect("serialises");
        let reopened = parse_project(&saved).expect("reopens");
        assert_eq!(reopened, parsed);
        assert_eq!(reopened.to_json().unwrap(), saved);
        assert_eq!(reopened.audio_sources, parsed.audio_sources);
    }

    /// A real version 2 project, written by the step 9 build and kept verbatim.
    ///
    /// The interesting one: version 2 had a piano roll, so this exercises the
    /// part of the migration that has something to translate.
    const V2_PROJECT: &str = include_str!("../tests/fixtures/v2.mydaw");

    #[test]
    fn a_version_two_project_keeps_its_grid_its_roll_and_its_channels() {
        assert!(
            V2_PROJECT.contains(r#""format_version": 2"#),
            "the fixture must stay a version 2 file"
        );
        assert!(
            V2_PROJECT.contains(r#""notes""#),
            "a version 2 file has a piano roll; this one must exercise it"
        );

        let parsed = parse_project(V2_PROJECT).expect("a version 2 project still opens");
        assert_eq!(parsed.format_version, FORMAT_VERSION);
        assert_eq!(parsed.to_bpm().unwrap(), 147.0);

        let (set, playlist) = parsed.to_song().expect("the song still runs");
        assert_eq!(set.len(), 1);
        let pattern = set.active();
        assert_eq!(pattern.id, 1);
        assert_eq!(pattern.name, "Pattern 1");
        assert_eq!(pattern.length_steps, 32, "two bars, as saved");

        // The flat rows became one clip per channel...
        assert_eq!(
            pattern
                .channels
                .iter()
                .map(|c| c.channel)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "one clip per channel the old rows addressed"
        );
        assert_eq!(pattern.hit_count(), 8 + 1 + 4);
        assert_eq!(
            pattern.channel(1).unwrap().steps.as_ref().unwrap().steps[4].velocity,
            1.0,
            "the accent survived"
        );

        // ...and the notes landed on the channels they named.
        assert_eq!(pattern.channel(1).unwrap().notes.len(), 2);
        assert_eq!(pattern.channel(2).unwrap().notes.len(), 1);
        assert_eq!(
            pattern.channel(2).unwrap().notes[0].start_ticks,
            1_920 + 320
        );
        assert_eq!(pattern.channel(1).unwrap().notes[1].velocity, 0.31);
        assert_eq!(pattern.note_count(), 3);

        // Version 2 had no arrangement and no mode.
        assert!(playlist.clips().is_empty());
        assert_eq!(parsed.to_play_mode(), PlayMode::Pattern);
    }

    #[test]
    fn a_second_row_on_one_channel_becomes_the_notes_it_always_meant() {
        // Legal in version 2 -- a drum-kit track had four rows on channel 0 --
        // and impossible in version 3, where a channel has one grid. The
        // migration turns the extra rows into notes, which is the same
        // translation `Pattern::compile` does, so the pattern still produces
        // exactly the events it did.
        let legacy = LegacyPatternDto {
            steps: 16,
            rows: vec![
                LegacyRowDto {
                    key: 36,
                    channel: 0,
                    label: String::new(),
                    steps: (0..16)
                        .map(|i| StepDto {
                            on: i == 0,
                            velocity: 0.78,
                        })
                        .collect(),
                },
                LegacyRowDto {
                    key: 42,
                    channel: 0,
                    label: String::new(),
                    steps: (0..16)
                        .map(|i| StepDto {
                            on: i == 8,
                            velocity: 1.0,
                        })
                        .collect(),
                },
            ],
            notes: Vec::new(),
        };
        let migrated = migrate_single_pattern(&legacy);
        assert_eq!(migrated.channels.len(), 1, "one channel, one clip");
        let clip = &migrated.channels[0];
        assert_eq!(
            clip.steps.as_ref().unwrap().key,
            36,
            "the first row keeps the grid"
        );
        assert_eq!(clip.notes.len(), 1, "the second row became a note");
        assert_eq!(clip.notes[0].key, 42);
        assert_eq!(clip.notes[0].start, 8 * TICKS_PER_STEP);
        assert_eq!(
            clip.notes[0].length,
            mydaw_plugin_sequencer::STEP_GATE_TICKS
        );
        assert_eq!(clip.notes[0].velocity, 1.0);

        // And the whole point: it compiles to the same two events.
        let compiled = Pattern::try_from(&migrated).unwrap().compile();
        assert_eq!(compiled.note_count(), 2);
        assert_eq!(compiled.note(0).unwrap().key, 36);
        assert_eq!(compiled.note(1).unwrap().key, 42);
        assert_eq!(compiled.note(1).unwrap().start_ticks, 8 * TICKS_PER_STEP);
    }

    #[test]
    fn a_piano_roll_survives_the_file_tick_for_tick() {
        // Ticks are integers and JSON numbers are doubles, so a note's position
        // is exact by construction -- but the *order* of the notes and the
        // pattern's length are not, and both are things a re-save could quietly
        // change.
        let file = captured();
        let parsed = parse_project(&file.to_json().unwrap()).unwrap();
        let (set, _) = parsed.to_song().unwrap();
        let pattern = set.get(1).expect("the beat");

        assert_eq!(pattern.length_steps, 32, "two bars, as saved");
        assert_eq!(*pattern, beat(), "note for note, in the order written");
        let triplet = pattern.channel(2).unwrap().notes[0];
        assert_eq!(triplet.start_ticks, 1_920 + 320);
        assert_eq!(
            triplet.start_ticks % TICKS_PER_STEP,
            80,
            "the triplet is still off the sixteenth grid, which is the point"
        );
        assert_eq!(pattern.channel(1).unwrap().notes[1].velocity, 0.31);
        assert_eq!(pattern.color.as_deref(), Some("#4f8ef7"));

        // The grids carry exactly the steps that play, not the engine's tail.
        assert!(parsed.patterns[0]
            .channels
            .iter()
            .filter_map(|clip| clip.steps.as_ref())
            .all(|row| row.steps.len() == 32));
    }

    #[test]
    fn a_four_bar_pattern_round_trips_at_its_own_length() {
        let mut pattern = Pattern::with_channels(1, "Pattern 1", &[0], |_| 36);
        pattern.length_steps = 64;
        pattern.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 32, 63]));
        pattern.channels[0].notes = vec![MelodicNote::new(60, 15_120, 240)];
        let set = PatternSet::single(pattern.clone());
        let file = ProjectFile::capture(
            &Project::default(),
            0,
            130.0,
            &Song {
                patterns: &set,
                playlist: &Playlist::default(),
                mode: PlayMode::Song,
                audio_sources: Vec::new(),
            },
            &[],
            None,
        );

        let text = file.to_json().unwrap();
        let parsed = parse_project(&text).unwrap();
        assert_eq!(parsed.patterns[0].steps, 64);
        assert_eq!(*parsed.to_song().unwrap().0.active(), pattern);
        assert_eq!(
            parsed.to_play_mode(),
            PlayMode::Song,
            "the mode is saved too"
        );
        assert_eq!(parsed.to_json().unwrap(), text, "and it is byte-stable");
    }

    #[test]
    fn an_arrangement_survives_the_file_clip_for_clip() {
        let parsed = parse_project(&captured().to_json().unwrap()).unwrap();
        let (_, playlist) = parsed.to_song().unwrap();
        assert_eq!(playlist.tracks().len(), 4);
        assert_eq!(playlist.tracks()[0].name, "Drums");
        assert!(playlist.tracks()[3].muted, "the mute is arrangement data");
        assert_eq!(playlist.clips().len(), 3);
        assert_eq!(playlist.end_ticks(), 8 * 3_840);
    }

    #[test]
    fn a_clip_naming_a_pattern_the_file_does_not_have_is_refused_on_load() {
        // Hand-edited, or written by a build that deleted the pattern without
        // dropping its clips. Loading it half-way would leave a silent gap that
        // only shows up on playback.
        let mut file = captured();
        file.playlist.clips[0].pattern_id = Some(999);
        let err = file.to_song().unwrap_err();
        assert!(matches!(err, ProjectFileError::Playlist(_)), "{err:?}");
        assert!(err.to_string().contains("999"), "{err}");
    }

    #[test]
    fn a_clip_of_a_kind_this_build_does_not_know_is_skipped_rather_than_fatal() {
        // A later format's clip. The rest of the arrangement still opens, which
        // is the same answer a missing plugin gets.
        let mut file = captured();
        file.playlist.clips[0].kind = "video".to_owned();
        let (_, playlist) = file.to_song().expect("the rest still loads");
        assert_eq!(playlist.clips().len(), 2);
    }

    #[test]
    fn a_pattern_length_the_engine_does_not_have_is_refused_on_load() {
        let mut file = captured();
        file.patterns[0].steps = 24;
        assert!(matches!(file.to_song(), Err(ProjectFileError::Pattern(_))));
        assert!(
            file.to_song()
                .unwrap_err()
                .to_string()
                .contains("16, 32 or 64"),
            "{}",
            file.to_song().unwrap_err()
        );
    }

    #[test]
    fn a_note_that_runs_off_the_end_of_the_pattern_is_refused_on_load() {
        // Hand-edited, or written by a build with a longer pattern and then
        // shortened. Either way the engine could not run it, and loading it
        // half-way would leave a note the scheduler owes an off it never sends.
        let mut file = captured();
        file.patterns[0].channels[1].notes[0].length = 99_999;
        let err = file.to_song().unwrap_err();
        assert!(matches!(err, ProjectFileError::Pattern(_)), "{err:?}");
        assert!(err.to_string().contains("ticks long"), "{err}");
    }

    #[test]
    fn corrupt_and_truncated_files_are_reported_rather_than_guessed_at() {
        let good = captured().to_json().unwrap();

        for (label, text) in [
            ("empty", String::new()),
            ("not json", "this is not a project".to_owned()),
            ("half a file", good[..good.len() / 2].to_owned()),
            ("one byte short", good[..good.len() - 2].to_owned()),
            ("an array", "[1, 2, 3]".to_owned()),
            ("null", "null".to_owned()),
        ] {
            let err = parse_project(&text).unwrap_err();
            assert!(
                matches!(err, ProjectFileError::Malformed(_)),
                "{label} produced {err:?}"
            );
        }

        // JSON, and an object, but not a project.
        let err = parse_project(r#"{"hello": "world"}"#).unwrap_err();
        assert!(err.to_string().contains("format_version"), "{err}");

        // The version is right and a field's type is not.
        let err = parse_project(r#"{"format_version": 1, "bpm": "fast"}"#).unwrap_err();
        assert!(matches!(err, ProjectFileError::Malformed(_)), "{err:?}");
    }

    #[test]
    fn a_project_with_no_tracks_or_too_many_is_refused() {
        let mut file = captured();
        file.tracks.clear();
        assert!(matches!(
            file.to_project(&EffectRegistry::new()),
            Err(ProjectFileError::NoTracks)
        ));

        let mut file = captured();
        let one = file.tracks[0].clone();
        file.tracks = vec![one; MAX_TRACKS + 1];
        assert!(matches!(
            file.to_project(&EffectRegistry::new()),
            Err(ProjectFileError::TooManyTracks(n)) if n == MAX_TRACKS + 1
        ));
    }

    #[test]
    fn a_track_the_mixer_would_refuse_names_itself_in_the_error() {
        for (field, mutate) in [
            (
                "gain",
                (|t: &mut TrackEntry| t.gain_db = 99.0) as fn(&mut TrackEntry),
            ),
            ("nan gain", |t| t.gain_db = f32::NAN),
            ("pan", |t| t.pan = 4.0),
            ("blob", |t| t.state_blob = "not base64!".to_owned()),
            ("unknown source", |t| {
                t.source = SourceEntry::Internal {
                    id: "wavetable".to_owned(),
                }
            }),
            ("bad class id", |t| {
                t.source = SourceEntry::Vst3(Vst3Entry {
                    module_path: r"C:\VST3\Thing.vst3".to_owned(),
                    cid: "nonsense".to_owned(),
                    name: String::new(),
                })
            }),
        ] {
            let mut file = captured();
            mutate(&mut file.tracks[1]);
            let err = file.to_project(&EffectRegistry::new()).unwrap_err();
            assert!(
                matches!(err, ProjectFileError::Track { track: 1, .. }),
                "{field} produced {err:?}"
            );
            assert!(err.to_string().contains("Fixture"), "{field}: {err}");
        }
    }

    #[test]
    fn a_master_effect_that_cannot_be_read_is_refused_as_the_master_effect() {
        let mut file = captured();
        file.master_effect.as_mut().unwrap().state_blob = "!!!!".to_owned();
        assert!(matches!(
            file.to_project(&EffectRegistry::new()),
            Err(ProjectFileError::MasterEffect(_))
        ));

        let mut file = captured();
        file.master_effect = Some(EffectEntry {
            plugin: EffectRef::vst3(r"C:\VST3\Thing.vst3", "not-hex", "Thing"),
            state_blob: String::new(),
        });
        let err = file.to_project(&EffectRegistry::new()).unwrap_err();
        assert!(matches!(err, ProjectFileError::MasterEffect(_)), "{err:?}");
        assert!(err.to_string().contains("class id"), "{err}");
    }

    #[test]
    fn a_tempo_or_a_pattern_the_engine_cannot_run_is_refused() {
        let mut file = captured();
        file.bpm = 5_000.0;
        assert!(matches!(
            file.to_bpm(),
            Err(ProjectFileError::InvalidBpm(_))
        ));
        assert!(file.validate().is_err(), "and a save refuses it too");

        let mut file = captured();
        file.patterns[0].channels[0]
            .steps
            .as_mut()
            .unwrap()
            .steps
            .pop();
        assert!(matches!(file.to_song(), Err(ProjectFileError::Pattern(_))));
    }

    #[test]
    fn a_non_finite_value_can_never_reach_a_file_this_build_could_not_reopen() {
        // `serde_json` writes NaN as `null`, and `null` will not read back as a
        // float -- so a save that let one through would produce a file MYDAW
        // itself could not open.
        let mut file = captured();
        file.tracks[0].gain_db = f32::NAN;
        assert!(file.to_json().is_err());

        let mut file = captured();
        file.master_gain_db = f32::INFINITY;
        assert!(file.to_json().is_err());

        let mut file = captured();
        file.bpm = f64::NAN;
        assert!(file.to_json().is_err());
    }

    // ------------------------------------------------------------ atomic write

    #[test]
    fn a_save_writes_through_a_sibling_and_renames_it_into_place() {
        let dir = TempDir::new("atomic");
        let path = dir.join("song.mydaw");
        write_project_file(&path, &captured()).expect("a fresh save");

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            captured().to_json().unwrap()
        );
        assert!(
            !temp_path_for(&path).exists(),
            "the sibling must not be left behind"
        );
        assert_eq!(read_project_file(&path).unwrap(), captured());
    }

    #[test]
    fn a_failed_save_leaves_the_previous_project_exactly_as_it_was() {
        // The one thing a save must never do. The failure is simulated by
        // occupying the sibling path with a directory, which is why that path is
        // deterministic and public.
        let dir = TempDir::new("atomic-fail");
        let path = dir.join("song.mydaw");
        write_project_file(&path, &captured()).expect("the first save");
        let original = fs::read_to_string(&path).unwrap();

        fs::create_dir_all(temp_path_for(&path)).expect("occupy the sibling");
        let mut doomed = captured();
        doomed.bpm = 96.0;
        doomed.tracks[0].name = "Should never be written".to_owned();
        let err = write_project_file(&path, &doomed).expect_err("the sibling is a directory");
        assert!(matches!(err, ProjectFileError::Io(_)), "{err:?}");

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "the project on disk survived a failed save byte for byte"
        );
        assert_eq!(read_project_file(&path).unwrap().bpm, 147.0);
    }

    #[test]
    fn a_save_replaces_an_existing_project_rather_than_appending_to_it() {
        // `fs::rename` over an existing file is the atomic replace on both
        // Windows and POSIX; a shorter second project must not leave a tail of
        // the first behind.
        let dir = TempDir::new("replace");
        let path = dir.join("song.mydaw");
        write_project_file(&path, &captured()).unwrap();

        let small = ProjectFile::capture(&Project::default(), 0, 130.0, &empty_song(), &[], None);
        write_project_file(&path, &small).unwrap();
        assert_eq!(read_project_file(&path).unwrap(), small);
        assert_eq!(fs::read_to_string(&path).unwrap(), small.to_json().unwrap());
    }

    #[test]
    fn reading_a_file_that_is_not_there_names_the_path() {
        let dir = TempDir::new("missing");
        let path = dir.join("nothing.mydaw");
        let err = read_project_file(&path).unwrap_err();
        assert!(matches!(err, ProjectFileError::Io(_)), "{err:?}");
        assert!(err.to_string().contains("nothing.mydaw"), "{err}");
    }

    #[test]
    fn the_sibling_lives_next_to_the_project_because_a_rename_is_one_volume() {
        let path = Path::new(r"D:\Songs\track.mydaw");
        let temp = temp_path_for(path);
        assert_eq!(temp.parent(), path.parent());
        assert_ne!(temp.file_name(), path.file_name());
        assert!(temp
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".tmp"));
        // A bare file name has no parent, and must still produce a sibling.
        assert_eq!(
            temp_path_for(Path::new("track.mydaw")).parent(),
            Some(Path::new(""))
        );
    }

    #[test]
    fn the_window_title_shows_the_file_name_without_its_extension() {
        assert_eq!(project_name(None), UNTITLED);
        assert_eq!(
            project_name(Some(Path::new(r"D:\Songs\My Track.mydaw"))),
            "My Track"
        );
        assert_eq!(project_name(Some(Path::new("track.mydaw"))), "track");
    }

    #[test]
    fn the_capture_clamps_a_selection_that_no_longer_exists() {
        let file = ProjectFile::capture(&Project::default(), 9, 130.0, &empty_song(), &[], None);
        assert_eq!(
            file.selected_track,
            Project::default().len() - 1,
            "a selection past the end lands on the last track"
        );
        // ...and a file that names one anyway is clamped on the way back in.
        let mut file = captured();
        let last = file.tracks.len() - 1;
        file.selected_track = 99;
        assert_eq!(file.to_project(&EffectRegistry::new()).unwrap().1, last);
    }

    // ------------------------------------------------------- version 4

    const V3_PROJECT: &str = include_str!("../tests/fixtures/v3.mydaw");

    /// A project with a bus, a send and an insert on every kind of chain.
    fn routed_session() -> Project {
        let registry = EffectRegistry::new();
        let mut project = Project::empty();
        project
            .add(Source::DrumKit, Some("Drums".to_owned()))
            .unwrap();
        project
            .add(Source::PolySynth, Some("Lead".to_owned()))
            .unwrap();
        project.master_gain_db = -1.5;

        project.add_bus(Some("Drum Bus".to_owned())).unwrap();
        project.add_bus(Some("Reverb".to_owned())).unwrap();
        project.buses[1].color = Some("#4f8ef7".to_owned());
        project.buses[0].gain_db = -2.5;
        project.buses[0].pan = 0.25;
        project.buses[1].muted = true;

        let send = project.add_send(StripId::Track(0), 0).unwrap();
        project
            .edit_send(send, |send| {
                send.level = 0.4;
                send.pre = true;
            })
            .unwrap();
        project.add_send(StripId::Bus(0), 1).unwrap();

        project
            .add_insert(InsertTarget::Track(0), "effects.eq", None, &registry)
            .unwrap();
        project
            .add_insert(InsertTarget::Bus(0), "effects.compressor", None, &registry)
            .unwrap();
        project
            .add_insert(InsertTarget::Master, "effects.utility", None, &registry)
            .unwrap();
        project.tracks[0].inserts[0]
            .params
            .set_by_id("low.gain", 6.0);
        project.buses[0].inserts[0].params.set_by_id("ratio", 8.0);
        project
    }

    fn routed_file(project: &Project) -> ProjectFile {
        let (patterns, playlist) = song();
        ProjectFile::capture(
            project,
            0,
            120.0,
            &Song {
                patterns: &patterns,
                playlist: &playlist,
                mode: PlayMode::Pattern,
                audio_sources: Vec::new(),
            },
            &[],
            None,
        )
    }

    #[test]
    fn a_version_three_project_still_opens_and_gains_empty_routing() {
        // The whole of the v3 -> v4 migration: `#[serde(default)]` gives a file
        // that predates buses exactly what it meant -- none.
        assert!(
            V3_PROJECT.contains(r#""format_version": 3"#),
            "the fixture must stay version 3; nobody may 'helpfully' update it"
        );
        assert!(
            !V3_PROJECT.contains(r#""buses""#) && !V3_PROJECT.contains(r#""inserts""#),
            "and it must predate the routing fields"
        );

        let parsed = parse_project(V3_PROJECT).expect("a version 3 project still opens");
        assert_eq!(
            parsed.format_version, FORMAT_VERSION,
            "migrate on load, write current on save"
        );
        assert!(parsed.buses.is_empty() && parsed.sends.is_empty());
        assert!(parsed.master_inserts.is_empty());
        assert!(parsed.tracks.iter().all(|track| track.inserts.is_empty()));

        // The version 3 content is untouched.
        assert_eq!(parsed.bpm, 147.0);
        assert_eq!(parsed.master_gain_db, -3.5);
        assert_eq!(parsed.play_mode, "song");
        assert_eq!(parsed.patterns.len(), 1);
        assert_eq!(parsed.playlist.clips.len(), 1);

        let (project, selected) = parsed
            .to_project(&EffectRegistry::new())
            .expect("and it describes a session this engine can run");
        assert_eq!(project.len(), 2);
        assert_eq!(selected, 1);
        assert!(project.buses.is_empty() && project.sends.is_empty());
        assert!(!project.has_inserts());

        // A save then writes version 4, with the new fields present rather than
        // omitted -- so the file says what it will sound like.
        let saved = parsed.to_json().unwrap();
        assert!(saved.contains(r#""format_version": 4"#), "{saved}");
        assert!(saved.contains(r#""buses": []"#) && saved.contains(r#""sends": []"#));
        assert!(saved.contains(r#""master_inserts": []"#));
        assert!(saved.contains(r#""inserts": []"#));

        // From there the ordinary byte-identity property holds.
        let reopened = parse_project(&saved).unwrap();
        assert_eq!(reopened, parsed);
        assert_eq!(reopened.to_json().unwrap(), saved);
    }

    #[test]
    fn a_project_file_from_the_future_is_refused_by_name() {
        let future = V3_PROJECT.replace(r#""format_version": 3"#, r#""format_version": 5"#);
        assert!(matches!(
            parse_project(&future),
            Err(ProjectFileError::FutureVersion { found: 5 })
        ));
    }

    #[test]
    fn buses_sends_and_insert_chains_round_trip_with_every_value_intact() {
        let project = routed_session();
        let file = routed_file(&project);
        let text = file.to_json().unwrap();
        let parsed = parse_project(&text).expect("what we wrote is what we read");
        assert_eq!(parsed, file);
        assert_eq!(
            parsed.to_json().unwrap(),
            text,
            "the round trip is byte-identical"
        );

        let (restored, _) = parsed.to_project(&EffectRegistry::new()).unwrap();
        assert_eq!(restored.buses.len(), 2);
        assert_eq!(restored.buses[0].name, "Drum Bus");
        assert_eq!(restored.buses[0].gain_db, -2.5);
        assert_eq!(restored.buses[0].pan, 0.25);
        assert!(restored.buses[1].muted);
        assert_eq!(restored.buses[1].color.as_deref(), Some("#4f8ef7"));

        assert_eq!(restored.sends.len(), 2);
        let send = &restored.sends[0];
        assert_eq!(send.source, StripId::Track(0));
        assert_eq!(send.dest, 0);
        assert_eq!(send.level, 0.4);
        assert!(send.pre);
        assert_eq!(restored.sends[1].source, StripId::Bus(0));

        // Parameter values survive, and so do the ids a UI keys its lists on.
        assert_eq!(
            restored.tracks[0].inserts[0].params.get_by_id("low.gain"),
            Some(6.0)
        );
        assert_eq!(
            restored.buses[0].inserts[0].params.get_by_id("ratio"),
            Some(8.0)
        );
        assert_eq!(restored.master_inserts[0].type_id, "effects.utility");
        assert_eq!(
            restored.tracks[0].inserts[0].id,
            project.tracks[0].inserts[0].id
        );
        assert_eq!(restored.next_insert_id(), project.next_insert_id());
        assert_eq!(restored.next_send_id(), project.next_send_id());
        assert_eq!(restored.next_bus_id(), project.next_bus_id());

        // The whole session compares equal, which is the one-line "nothing was
        // lost" the byte-identity test rests on.
        assert_eq!(restored, project);
    }

    #[test]
    fn every_parameter_is_written_not_only_the_ones_that_were_moved() {
        // A default that changes in a later build must not silently change a
        // saved project.
        let project = routed_session();
        let file = routed_file(&project);
        let entry = &file.tracks[0].inserts[0];
        assert_eq!(entry.type_id, "effects.eq");
        assert_eq!(
            entry.params.len(),
            EffectRegistry::new()
                .info("effects.eq")
                .unwrap()
                .params
                .len()
        );
        // A `BTreeMap`, so the key order on disk is deterministic.
        let keys: Vec<&String> = entry.params.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn an_effect_this_build_does_not_have_is_skipped_rather_than_fatal() {
        // The missing-plugin policy, one level down: the project opens, that
        // effect is simply not in the chain, and the entry is still in the file.
        let project = routed_session();
        let mut file = routed_file(&project);
        file.tracks[0].inserts.push(InsertEntry {
            id: 99,
            type_id: "effects.not-an-effect".to_owned(),
            params: BTreeMap::new(),
        });
        let (restored, _) = file
            .to_project(&EffectRegistry::new())
            .expect("an unknown effect must not refuse the project");
        assert_eq!(restored.tracks[0].inserts.len(), 1);
        assert_eq!(restored.tracks[0].inserts[0].type_id, "effects.eq");
    }

    #[test]
    fn a_parameter_this_build_does_not_have_is_ignored_at_parameter_granularity() {
        let project = routed_session();
        let mut file = routed_file(&project);
        file.tracks[0].inserts[0]
            .params
            .insert("low.resonance".to_owned(), 0.7);
        let (restored, _) = file.to_project(&EffectRegistry::new()).unwrap();
        assert_eq!(
            restored.tracks[0].inserts[0].params.get_by_id("low.gain"),
            Some(6.0),
            "the parameters this build does have still arrive"
        );
    }

    #[test]
    fn a_hand_edited_file_that_routes_a_bus_into_itself_opens_with_that_edge_dropped() {
        // A cycle cannot be created through the command surface, so this can
        // only come from a hand-edited file. Dropping the edge that closes the
        // loop keeps the project openable, which is worth far more than
        // refusing it over a routing typo.
        let project = routed_session();
        let mut file = routed_file(&project);
        file.sends.push(SendEntry {
            id: 90,
            source_kind: "bus".to_owned(),
            source_index: 1,
            dest: 0,
            level: 1.0,
            pre: false,
        });
        // ...and a self-send, and one naming a strip that is not there.
        file.sends.push(SendEntry {
            id: 91,
            source_kind: "bus".to_owned(),
            source_index: 0,
            dest: 0,
            level: 1.0,
            pre: false,
        });
        file.sends.push(SendEntry {
            id: 92,
            source_kind: "track".to_owned(),
            source_index: 9,
            dest: 0,
            level: 1.0,
            pre: false,
        });
        file.sends.push(SendEntry {
            id: 93,
            source_kind: "aux".to_owned(),
            source_index: 0,
            dest: 0,
            level: 1.0,
            pre: false,
        });

        let (restored, _) = file.to_project(&EffectRegistry::new()).unwrap();
        assert_eq!(
            restored.sends.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![0, 1],
            "only the two real taps survive"
        );
    }

    #[test]
    fn a_file_with_more_buses_than_the_engine_runs_is_refused_by_name() {
        let project = routed_session();
        let mut file = routed_file(&project);
        for id in 0..MAX_BUSES as u32 + 1 {
            file.buses.push(BusEntry {
                id: 100 + id,
                name: format!("Extra {id}"),
                color: None,
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                inserts: Vec::new(),
            });
        }
        assert!(matches!(
            file.to_project(&EffectRegistry::new()),
            Err(ProjectFileError::TooManyBuses(_))
        ));
    }
}
