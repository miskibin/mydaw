//! Wire shapes owned by the shell.
//!
//! The engine crates define the canonical Rust types; this module exists where
//! the TypeScript contract in `ui/src/api/types.ts` disagrees with serde's
//! default rendering of them, or where a type needs a shape the engine has no
//! business knowing about. [`MeterFrame`] serialises its fields as
//! `peak_l`/`sample_pos` while the UI reads `peakL`/`samplePos`; a
//! [`Pattern`] is a fixed-size array the webview would rather see as a list,
//! with a row label attached. Pushing either convention down into crates that
//! are deliberately Tauri-free and webview-agnostic would be the wrong trade, so
//! the translation lives here, at the boundary that actually cares.

use mydaw_ipc_bridge::{MeterFrame, TrackMeter};
use mydaw_plugin_host_clap::ClapPluginInfo;
use mydaw_plugin_host_vst3::Vst3PluginInfo;
use mydaw_plugin_sequencer::{
    AudioSource, ChannelClip, Clip, MelodicNote, Pattern, PatternError, PatternSet, Playlist,
    PlaylistTrack, Step, StepRow, DEFAULT_STEP_VELOCITY, MAX_CLIPS, MAX_PATTERNS,
    MAX_PLAYLIST_TRACKS, MAX_SONG_TICKS, MAX_STEPS, MAX_WAVEFORM_BINS,
};
use serde::{Deserialize, Serialize};

/// One track's level, as the mixer panel draws it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackMeterDto {
    pub peak_l: f32,
    pub peak_r: f32,
}

impl From<&TrackMeter> for TrackMeterDto {
    fn from(meter: &TrackMeter) -> Self {
        Self {
            peak_l: meter.peak_l,
            peak_r: meter.peak_r,
        }
    }
}

/// [`MeterFrame`] in the shape the webview expects (camelCase).
///
/// The master fields are flat and `Copy`; the per-track levels become a `Vec` of
/// exactly the tracks that exist, because a JSON array of sixteen entries with
/// fourteen of them zero is fourteen entries of nothing crossing the boundary
/// thirty times a second. The RT side stays a fixed array -- see
/// [`MeterFrame::tracks`] -- and this is the one place the two shapes differ.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterFrameDto {
    pub peak_l: f32,
    pub peak_r: f32,
    pub rms_l: f32,
    pub rms_r: f32,
    /// Transport position in frames at the end of the measured block.
    pub sample_pos: u64,
    pub playing: bool,
    /// Post-strip, pre-master peak of every track, in track order.
    pub tracks: Vec<TrackMeterDto>,
    /// When the pump handed this frame to the webview, on the bench clock
    /// ([`crate::ipc_bench::epoch_ms`]).
    ///
    /// `None` -- and absent from the JSON entirely -- unless the IPC bench asked
    /// the pump to stamp frames, which is the only thing that reads it. That
    /// keeps the normal meter frame byte-for-byte what it was before spike 4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pump_ms: Option<f64>,
}

impl From<MeterFrame> for MeterFrameDto {
    fn from(frame: MeterFrame) -> Self {
        Self {
            peak_l: frame.peak_l,
            peak_r: frame.peak_r,
            rms_l: frame.rms_l,
            rms_r: frame.rms_r,
            sample_pos: frame.sample_pos,
            playing: frame.playing,
            tracks: frame
                .track_meters()
                .iter()
                .map(TrackMeterDto::from)
                .collect(),
            pump_ms: None,
        }
    }
}

/// One row of the CLAP browser panel.
///
/// `path` is a `String`, not a `PathBuf`: serde renders a `PathBuf` as whatever
/// the OS encoding happens to be, and a Windows path with a non-UTF-8 byte would
/// fail to serialise and take the whole command with it. Lossy conversion here
/// means a mangled character in a tooltip instead of an empty panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClapPluginInfoDto {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub path: String,
    /// What the plugin advertises itself as: `instrument`, `audio-effect`,
    /// `note-effect` or `other`. The browser badges it, and the pickers filter
    /// on it -- an effect cannot head a track and an instrument cannot process
    /// the master bus.
    ///
    /// Read from the descriptor's `features`, which is free; the authoritative
    /// check is the plugin's port layout, and it happens when an instance is
    /// created. See
    /// [`ClapPluginKind`](mydaw_plugin_host_clap::ClapPluginKind).
    pub kind: String,
    /// The declared feature strings, verbatim. Shown as a tooltip, and the raw
    /// material for a smarter filter later.
    pub features: Vec<String>,
}

impl From<&ClapPluginInfo> for ClapPluginInfoDto {
    fn from(info: &ClapPluginInfo) -> Self {
        Self {
            id: info.id.clone(),
            name: info.name.clone(),
            vendor: info.vendor.clone(),
            version: info.version.clone(),
            path: info.path.to_string_lossy().into_owned(),
            kind: info.kind().id().to_owned(),
            features: info.features.clone(),
        }
    }
}

/// One row of the VST3 browser panel.
///
/// The counterpart of [`ClapPluginInfoDto`], and deliberately close to it so one
/// UI list can show both with a format badge. The differences are the format's:
/// `id` is a 32-digit class id rather than a reverse-DNS string, and
/// `subcategories` is one `|`-separated string rather than a list -- carried
/// verbatim, for the same reason CLAP's `features` are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Vst3PluginInfoDto {
    /// Class id as 32 uppercase hex digits. Unique within a module, and the
    /// half of a locator a project file stores next to the path.
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// Absolute path of the `.vst3` module (bundle directory or flat DLL).
    pub path: String,
    /// Which picker this plugin belongs in: `instrument`, `audio-effect`,
    /// `other`, or `unknown`.
    ///
    /// Shares its vocabulary with [`ClapPluginInfoDto::kind`] so one list can
    /// filter both formats. `unknown` has no CLAP equivalent: it means the module
    /// exposes no `IPluginFactory2` and therefore declares no subcategories at
    /// all, which is a real state and not a failure.
    pub kind: String,
    /// The declared subcategories, split for display (`["Fx", "Reverb"]`).
    pub subcategories: Vec<String>,
}

impl From<&Vst3PluginInfo> for Vst3PluginInfoDto {
    fn from(info: &Vst3PluginInfo) -> Self {
        Self {
            id: info.cid.to_hex(),
            name: info.name.clone(),
            vendor: info.vendor.clone(),
            version: info.version.clone(),
            path: info.path.to_string_lossy().into_owned(),
            kind: info.kind().id().to_owned(),
            subcategories: info.subcategory_list(),
        }
    }
}

// ---------------------------------------------------------------- sequencer
//
// The wire shapes the pattern panels, the piano roll and the playlist read and
// write. Two conventions run through all of them:
//
// **camelCase out, camelCase in.** `#[serde(rename_all = "camelCase")]`
// everywhere, because the consumer is TypeScript.
//
// **Ids, not indices.** A pattern, a playlist lane and a clip each carry a
// stable `u64`. Positions change under insertion and deletion; a UI keyed on
// one animates wrongly and a clip addressed by one silently repoints.

/// One cell of the step grid.
///
/// `velocity` is carried even for a cell that is off, so toggling a step in the
/// UI and toggling it back does not silently reset an accent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepDto {
    pub on: bool,
    /// Normalised velocity, `0.0..=1.0`.
    #[serde(default = "default_velocity")]
    pub velocity: f32,
}

fn default_velocity() -> f32 {
    DEFAULT_STEP_VELOCITY
}

impl From<&Step> for StepDto {
    fn from(step: &Step) -> Self {
        Self {
            on: step.on,
            velocity: step.velocity,
        }
    }
}

/// One channel's step grid.
///
/// `label` is derived, not stored: the row's identity is its key, and the name
/// ("Kick", "CH") is a rendering of that key. It is sent so the UI does not have
/// to keep a second copy of the General MIDI drum map, and ignored on the way
/// back in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepRowDto {
    /// MIDI key every switched-on cell triggers.
    pub key: u8,
    #[serde(default)]
    pub label: String,
    /// Exactly [`PatternDto::steps`] entries -- the cells that play, not the
    /// engine's full-length array. See [`Pattern::try_from`].
    pub steps: Vec<StepDto>,
}

impl StepRowDto {
    /// This row's first `length` cells, which are the ones the pattern plays.
    pub fn of(row: &StepRow, length: usize) -> Self {
        Self {
            key: row.key,
            label: row.label(),
            steps: row
                .steps
                .iter()
                .take(length.min(MAX_STEPS))
                .map(StepDto::from)
                .collect(),
        }
    }
}

/// One note of the piano roll.
///
/// `start` and `length` are in **ticks** -- 960 to the quarter note, so a
/// sixteenth is 240. Named without the unit in the field itself because this
/// shape is written into the project file as well as sent to the webview, and
/// `startTicks` in a file whose every other key is short would be a wart on the
/// format for the rest of its life. The units are documented here, in
/// `ui/src/api/types.ts`, and in the spec.
///
/// **No channel.** The note lives inside a [`ChannelClipDto`], which knows which
/// channel it is; carrying it twice was two facts that could disagree.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MelodicNoteDto {
    /// MIDI key, `0..=127`. 60 is middle C.
    pub key: u8,
    /// Offset from the start of the pattern, in ticks.
    pub start: u32,
    /// How long the note sounds, in ticks. At least 1.
    pub length: u32,
    #[serde(default = "default_velocity")]
    pub velocity: f32,
}

impl From<&MelodicNote> for MelodicNoteDto {
    fn from(note: &MelodicNote) -> Self {
        Self {
            key: note.key,
            start: note.start_ticks,
            length: note.length_ticks,
            velocity: note.velocity,
        }
    }
}

impl From<&MelodicNoteDto> for MelodicNote {
    fn from(dto: &MelodicNoteDto) -> Self {
        Self {
            key: dto.key,
            start_ticks: dto.start,
            length_ticks: dto.length,
            velocity: dto.velocity,
        }
    }
}

/// What one pattern says to one channel.
///
/// `mode` is **derived** from whether there are notes (see
/// [`ChannelClip::display_mode`]), and is sent so the UI does not have to
/// re-derive the rule. It is ignored on the way in: the content decides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelClipDto {
    /// The rack channel, the mixer strip and the note channel: one index.
    pub channel: u8,
    /// `steps` or `roll`. Derived; ignored on the way in.
    #[serde(default)]
    pub mode: String,
    /// The step grid, when this channel has one. `null` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<StepRowDto>,
    #[serde(default)]
    pub notes: Vec<MelodicNoteDto>,
}

impl ChannelClipDto {
    /// One clip, with its grid trimmed to the pattern's playing length.
    pub fn of(clip: &ChannelClip, length: usize) -> Self {
        Self {
            channel: clip.channel,
            mode: clip.display_mode().id().to_owned(),
            steps: clip.steps.as_ref().map(|row| StepRowDto::of(row, length)),
            notes: clip.notes.iter().map(MelodicNoteDto::from).collect(),
        }
    }
}

/// A whole pattern, as the grid and the roll read and write it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternDto {
    /// Stable across a save and load. Playlist clips reference it.
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
    /// CSS colour the UI draws this pattern's clips in. Absent means "the UI
    /// picks" -- the engine has no palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Steps in the loop: 16, 32 or 64. Every step row carries this many cells.
    pub steps: u8,
    #[serde(default)]
    pub channels: Vec<ChannelClipDto>,
}

impl From<&Pattern> for PatternDto {
    fn from(pattern: &Pattern) -> Self {
        let length = pattern.length_steps as usize;
        Self {
            id: pattern.id,
            name: pattern.name.clone(),
            color: pattern.color.clone(),
            steps: pattern.length_steps,
            channels: pattern
                .channels
                .iter()
                .map(|clip| ChannelClipDto::of(clip, length))
                .collect(),
        }
    }
}

impl TryFrom<&PatternDto> for Pattern {
    type Error = PatternError;

    /// Rebuild a [`Pattern`] from what the webview sent, then validate it.
    ///
    /// A step row whose `steps` array is not exactly `dto.steps` long is
    /// rejected here rather than padded or truncated: a UI that sent 15 steps
    /// has a bug, and quietly inventing the sixteenth would hide it behind a
    /// beat that is almost right.
    ///
    /// Note that the *engine's* rows are always [`MAX_STEPS`] long whatever the
    /// pattern's length -- the tail is kept so that shortening a pattern is not
    /// destructive -- but the wire only ever carries the steps that play. A
    /// round trip through the webview therefore loses the tail, which is the
    /// right trade: the UI has no way to show it, and sending 64 cells to draw
    /// 16 of them would be inviting a UI bug to become a data one. The commands
    /// that *change* a pattern's length go through
    /// [`PatternSet::set_length`](mydaw_plugin_sequencer::PatternSet::set_length),
    /// which does keep it.
    fn try_from(dto: &PatternDto) -> Result<Self, Self::Error> {
        let length = dto.steps as usize;
        if !Pattern::is_legal_length(length) {
            return Err(PatternError::WrongStepCount(length));
        }
        let mut channels = Vec::with_capacity(dto.channels.len());
        for (index, clip) in dto.channels.iter().enumerate() {
            let steps = match &clip.steps {
                Some(row) => {
                    if row.steps.len() != length {
                        return Err(PatternError::WrongRowLength {
                            clip: index,
                            got: row.steps.len(),
                            expected: length,
                        });
                    }
                    let mut cells = [Step::OFF; MAX_STEPS];
                    for (cell, source) in cells.iter_mut().zip(&row.steps) {
                        *cell = Step {
                            on: source.on,
                            velocity: source.velocity,
                        };
                    }
                    Some(StepRow {
                        key: row.key,
                        steps: cells,
                    })
                }
                None => None,
            };
            channels.push(ChannelClip {
                channel: clip.channel,
                steps,
                notes: clip.notes.iter().map(MelodicNote::from).collect(),
            });
        }
        let pattern = Self {
            id: dto.id,
            name: if dto.name.trim().is_empty() {
                format!("Pattern {}", dto.id)
            } else {
                dto.name.clone()
            },
            color: dto.color.clone(),
            length_steps: dto.steps,
            channels,
        };
        pattern.validate()?;
        Ok(pattern)
    }
}

/// One row of the pattern picker.
///
/// Everything a list needs and nothing a grid does: the counts are derived here
/// so a picker showing "16 events" does not have to fetch the whole pattern to
/// find out.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternSummaryDto {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Steps in the loop: 16, 32 or 64.
    pub steps: u8,
    /// Ticks in one time round the loop. `steps * 240`.
    pub length_ticks: u32,
    /// Channels this pattern says anything about.
    pub channel_count: usize,
    /// Switched-on steps inside the loop, across every channel.
    pub hit_count: usize,
    /// Melodic notes, across every channel.
    pub note_count: usize,
}

impl From<&Pattern> for PatternSummaryDto {
    fn from(pattern: &Pattern) -> Self {
        Self {
            id: pattern.id,
            name: pattern.name.clone(),
            color: pattern.color.clone(),
            steps: pattern.length_steps,
            length_ticks: pattern.ticks(),
            channel_count: pattern.channels.len(),
            hit_count: pattern.hit_count(),
            note_count: pattern.note_count(),
        }
    }
}

/// The pattern picker's whole state, plus the pattern being edited.
///
/// The summaries and the full active pattern travel together because every
/// panel that draws one draws the other: switching pattern has to repaint the
/// picker's selection *and* the grid, and two round trips would let them
/// disagree for a frame.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternSetDto {
    pub patterns: Vec<PatternSummaryDto>,
    /// Id of the pattern being edited. Always names one of `patterns`.
    pub active_id: u64,
    /// That pattern, in full.
    pub active: PatternDto,
    /// Most patterns a project may hold, so the UI can grey out "add".
    pub max_patterns: usize,
}

impl From<&PatternSet> for PatternSetDto {
    fn from(set: &PatternSet) -> Self {
        Self {
            patterns: set.patterns().iter().map(PatternSummaryDto::from).collect(),
            active_id: set.active_id(),
            active: PatternDto::from(set.active()),
            max_patterns: MAX_PATTERNS,
        }
    }
}

/// One lane of the arrangement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistTrackDto {
    pub id: u64,
    pub name: String,
    pub muted: bool,
}

impl From<&PlaylistTrack> for PlaylistTrackDto {
    fn from(track: &PlaylistTrack) -> Self {
        Self {
            id: track.id,
            name: track.name.clone(),
            muted: track.muted,
        }
    }
}

/// One clip on the timeline.
///
/// One shape for both kinds rather than a tagged union, because the UI draws
/// them with the same geometry and only the fill differs: `kind` says which,
/// and the fields the other kind does not have are simply absent from the JSON.
/// A discriminated union would make every consumer narrow a type to read
/// `start`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipDto {
    pub id: u64,
    /// `pattern` or `audio`.
    pub kind: &'static str,
    /// Index of the lane it is drawn on.
    pub track: usize,
    /// Where it starts, in absolute song ticks.
    pub start: u64,
    /// How long it plays for, in song ticks.
    pub length: u64,
    /// The pattern it plays. Absent for an audio clip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_id: Option<u64>,
    /// The loaded audio file it plays. Absent for a pattern clip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<u64>,
    /// How far into that file playback begins, in frames of the file's own
    /// rate. Absent for a pattern clip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_frames: Option<u64>,
    /// Per-clip trim in dB. Absent for a pattern clip, whose channels have
    /// faders of their own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f32>,
}

impl From<&Clip> for ClipDto {
    fn from(clip: &Clip) -> Self {
        let base = Self {
            id: clip.id(),
            kind: clip.kind(),
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
}

/// The arrangement.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDto {
    pub tracks: Vec<PlaylistTrackDto>,
    pub clips: Vec<ClipDto>,
    /// One past the last tick any clip occupies. `0` for an empty arrangement,
    /// and where SONG mode stops.
    pub end_ticks: u64,
    pub max_tracks: usize,
    pub max_clips: usize,
    /// Furthest a clip may run, in ticks.
    pub max_ticks: u64,
}

impl From<&Playlist> for PlaylistDto {
    fn from(playlist: &Playlist) -> Self {
        Self {
            tracks: playlist
                .tracks()
                .iter()
                .map(PlaylistTrackDto::from)
                .collect(),
            clips: playlist.clips().iter().map(ClipDto::from).collect(),
            end_ticks: playlist.end_ticks(),
            max_tracks: MAX_PLAYLIST_TRACKS,
            max_clips: MAX_CLIPS,
            max_ticks: MAX_SONG_TICKS,
        }
    }
}

/// One loaded audio file, as the browser and the clip inspector read it.
///
/// Everything here is a property of the *file*, not of any clip that plays it:
/// two clips of one take show the same row. The waveform is deliberately **not**
/// in it -- see [`WaveformDto`] -- because a source list is fetched on every
/// arrangement change and a list of ten takes would carry a megabyte of peaks
/// nobody asked for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSourceDto {
    pub id: u64,
    /// Where the file was when it was loaded.
    pub path: String,
    /// Its file name, for a UI with no room for the path.
    pub name: String,
    /// Playable frames, in the file's own rate. `0` for a file that has moved.
    pub frames: u64,
    /// The rate the file was recorded at, which need not be the device's.
    /// `0` for a file that has moved.
    pub sample_rate: f64,
    pub duration_seconds: f64,
    /// How long the file is at the session's **current** tempo, in ticks --
    /// which is what a clip drawn to cover the whole take should be `length`.
    /// It moves with the tempo, because a tick does; the audio does not stretch.
    pub duration_ticks: u64,
    /// Whether the file could not be found or read. Its clips stay in the
    /// arrangement and are silent -- see the sequencer's `audio` module.
    pub missing: bool,
    /// Bytes of audio this source is holding.
    pub bytes: u64,
}

impl AudioSourceDto {
    /// Describe `source` at `bpm`.
    ///
    /// The tempo is a parameter rather than a field of the source because a file
    /// has no tempo: how many ticks it covers is a question about the *session*,
    /// and the answer changes when the session's tempo does.
    pub fn of(source: &AudioSource, bpm: f64) -> Self {
        Self {
            id: source.id(),
            path: source.path().to_string_lossy().into_owned(),
            name: source.name(),
            frames: source.frames() as u64,
            sample_rate: source.sample_rate(),
            duration_seconds: source.seconds(),
            duration_ticks: ticks_for_seconds(source.seconds(), bpm),
            missing: source.is_missing(),
            bytes: source.bytes() as u64,
        }
    }
}

/// Ticks that `seconds` of wall-clock time covers at `bpm`.
///
/// `PPQ` ticks per beat, `bpm` beats per minute. Rounded up so a clip drawn to
/// the whole of a take never ends one tick before the last sample.
pub fn ticks_for_seconds(seconds: f64, bpm: f64) -> u64 {
    if !(seconds.is_finite() && bpm.is_finite()) || seconds <= 0.0 || bpm <= 0.0 {
        return 0;
    }
    (seconds * bpm / 60.0 * f64::from(mydaw_plugin_sequencer::PPQ)).ceil() as u64
}

/// A source's waveform, at the resolution that was asked for.
///
/// `peaks` is one `[min, max]` pair per bin, over **both** channels: a clip on a
/// timeline is drawn as one envelope at the sizes this is for, and the outer
/// bound never hides a transient that is on one channel only. Pairs rather than
/// two arrays because a consumer reads them together, and `[[min, max], ...]`
/// is what a `<path>` builder wants.
///
/// # Why JSON and not a binary blob
/// The IPC spike measured a 64 kB message as free, and
/// [`MAX_WAVEFORM_BINS`](mydaw_plugin_sequencer::MAX_WAVEFORM_BINS) pairs is
/// roughly 40 kB of JSON -- inside it, for a payload a UI fetches once per
/// source rather than per frame. A base64 `Float32Array` would be about a third
/// of the size and would cost every consumer a decode step; the seam to add it
/// later is this type, and nothing else would change.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveformDto {
    pub source_id: u64,
    /// Frames the whole waveform spans.
    pub frames: u64,
    /// Pairs actually returned, which may be fewer than asked for when the file
    /// has less detail than that.
    pub bins: usize,
    /// The most bins this build will ever return.
    pub max_bins: usize,
    pub peaks: Vec<[f32; 2]>,
}

impl WaveformDto {
    /// `target_bins` pairs spanning the whole of `source`.
    pub fn of(source: &AudioSource, target_bins: usize) -> Self {
        let peaks: Vec<[f32; 2]> = source
            .waveform(target_bins)
            .into_iter()
            .map(|(min, max)| [min, max])
            .collect();
        Self {
            source_id: source.id(),
            frames: source.frames() as u64,
            bins: peaks.len(),
            max_bins: MAX_WAVEFORM_BINS,
            peaks,
        }
    }
}

/// Everything the pattern panels, the playlist and the transport bar need in
/// one message.
///
/// One message rather than four because every one of them needs the tempo and
/// the tick geometry to draw itself, and four round trips would let the picker,
/// the grid and the timeline disagree about which pattern is active for a frame.
/// The IPC spike measured a message this size as free.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequencerStateDto {
    /// Session tempo, already clamped to the transport's legal range.
    pub bpm: f64,
    /// The open device's sample rate, or `0` when no stream is running.
    ///
    /// The panels need it to turn the transport position they get in *frames*
    /// into a tick. Without it the UI would have to assume 48 kHz, and the
    /// moving playhead would sit in the wrong place on a 44.1 kHz interface --
    /// visible, and exactly the kind of small lie that erodes trust in a
    /// playhead.
    pub sample_rate: f64,
    /// `pat` or `song`.
    pub play_mode: &'static str,
    /// Ticks per quarter note. Sent rather than hard-coded in the UI so there is
    /// one definition of the grid.
    pub ppq: u32,
    /// Ticks in one step of the grid (a sixteenth).
    pub ticks_per_step: u32,
    /// Ticks in one bar of 4/4.
    pub ticks_per_bar: u32,
    pub pattern_set: PatternSetDto,
    pub playlist: PlaylistDto,
}

/// What the open audio device is, for the status bar's BUFFER and LATENCY.
///
/// Every field is `0`/empty when no stream is running, which is a real state
/// rather than a failure: the UI shows a dash and says so. `latency_ms` is
/// derived here rather than in the webview so the two never disagree about
/// which block size the figure was computed from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfoDto {
    /// The device the stream is on, or an empty string while stopped.
    pub device_name: String,
    pub sample_rate: f64,
    pub channels: u32,
    /// Largest block the callback can be handed, in frames.
    pub buffer_frames: u32,
    /// One block's worth of time, in milliseconds. `0` while stopped.
    pub latency_ms: f64,
    /// Whether a stream is open at all.
    pub running: bool,
}

impl StreamInfoDto {
    /// The "no device" answer: what the status bar draws before audio starts.
    pub fn stopped() -> Self {
        Self {
            device_name: String::new(),
            sample_rate: 0.0,
            channels: 0,
            buffer_frames: 0,
            latency_ms: 0.0,
            running: false,
        }
    }
}

impl From<mydaw_plugin_audio_io::StreamInfo> for StreamInfoDto {
    fn from(info: mydaw_plugin_audio_io::StreamInfo) -> Self {
        let sample_rate = f64::from(info.sample_rate);
        let buffer_frames = info.max_block as u32;
        let latency_ms = if sample_rate > 0.0 {
            f64::from(buffer_frames) / sample_rate * 1000.0
        } else {
            0.0
        };
        Self {
            device_name: info.device_name,
            sample_rate,
            channels: u32::from(info.channels),
            buffer_frames,
            latency_ms,
            running: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_plugin_sequencer::{Playlist, DEFAULT_STEPS, TICKS_PER_BAR};

    const BAR: u64 = TICKS_PER_BAR as u64;

    fn starter() -> PatternSet {
        PatternSet::starter(&[0, 1, 2, 3, 4], |channel| {
            mydaw_plugin_sequencer::DEFAULT_ROW_KEYS
                .get(channel as usize)
                .copied()
                .unwrap_or(60)
        })
    }

    #[test]
    fn serialises_with_the_key_names_the_ui_reads() {
        let dto = MeterFrameDto::from(MeterFrame {
            peak_l: 0.5,
            peak_r: 0.25,
            rms_l: 0.1,
            rms_r: 0.2,
            sample_pos: 4_096,
            playing: true,
            ..MeterFrame::SILENT
        });
        let json: serde_json::Value = serde_json::to_value(dto).unwrap();
        let object = json.as_object().unwrap();

        // Mirrors `MeterFrame` in ui/src/api/types.ts, key for key.
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "peakL",
                "peakR",
                "playing",
                "rmsL",
                "rmsR",
                "samplePos",
                "tracks"
            ]
        );
        assert_eq!(object["peakL"], 0.5);
        assert_eq!(object["samplePos"], 4_096);
        assert_eq!(object["playing"], true);
        assert_eq!(object["tracks"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn only_the_tracks_that_exist_cross_the_boundary() {
        let mut peaks = mydaw_rt_core::TrackPeaks::empty(2);
        peaks.set(
            1,
            mydaw_rt_core::TrackPeak {
                peak_l: 0.75,
                peak_r: 0.5,
            },
        );
        let mut frame = MeterFrame::SILENT;
        frame.set_track_peaks(&peaks);

        let dto = MeterFrameDto::from(frame);
        assert_eq!(dto.tracks.len(), 2, "not the fixed sixteen");
        assert_eq!(
            dto.tracks[0],
            TrackMeterDto {
                peak_l: 0.0,
                peak_r: 0.0
            }
        );
        assert_eq!(dto.tracks[1].peak_l, 0.75);

        let json: serde_json::Value = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["tracks"][1]["peakL"], 0.75);
        assert_eq!(json["tracks"][1]["peakR"], 0.5);
    }

    #[test]
    fn the_bench_timestamp_is_absent_unless_the_pump_stamped_the_frame() {
        let plain = MeterFrameDto::from(MeterFrame::SILENT);
        assert_eq!(plain.pump_ms, None);
        let json = serde_json::to_string(&plain).unwrap();
        assert!(
            !json.contains("pumpMs"),
            "an unstamped frame carries nothing the bench added: {json}"
        );

        let stamped = MeterFrameDto {
            pump_ms: Some(1234.5),
            ..plain
        };
        let json: serde_json::Value = serde_json::to_value(stamped).unwrap();
        assert_eq!(json["pumpMs"], 1234.5);
    }

    #[test]
    fn clap_plugin_info_serialises_with_the_keys_the_ui_reads() {
        let dto = ClapPluginInfoDto::from(&ClapPluginInfo {
            id: "com.acme.thing".to_owned(),
            name: "Thing".to_owned(),
            vendor: "Acme".to_owned(),
            version: "1.2.3".to_owned(),
            path: std::path::PathBuf::from(r"C:\CLAP\Thing.clap"),
            features: vec!["instrument".to_owned(), "stereo".to_owned()],
        });
        let json: serde_json::Value = serde_json::to_value(dto).unwrap();
        let object = json.as_object().unwrap();

        // Mirrors `ClapPluginInfoDto` in ui/src/api/types.ts, key for key.
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["features", "id", "kind", "name", "path", "vendor", "version"]
        );
        assert_eq!(object["id"], "com.acme.thing");
        assert_eq!(object["name"], "Thing");
        assert_eq!(object["vendor"], "Acme");
        assert_eq!(object["version"], "1.2.3");
        assert_eq!(object["path"], r"C:\CLAP\Thing.clap");
        assert_eq!(
            object["kind"], "instrument",
            "the picker filters on this, so it has to come from the features"
        );
    }

    #[test]
    fn a_plugin_with_no_vendor_or_version_still_serialises() {
        // `scan_bundle` leaves these empty rather than dropping the plugin, so
        // the DTO has to survive it -- the UI shows a dash.
        let dto = ClapPluginInfoDto::from(&ClapPluginInfo {
            id: "x".to_owned(),
            name: String::new(),
            vendor: String::new(),
            version: String::new(),
            path: std::path::PathBuf::from("x.clap"),
            features: Vec::new(),
        });
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"vendor\":\"\""), "{json}");
        assert_eq!(
            dto.kind, "other",
            "a bundle that declares no features is not a lie, just unhelpful"
        );
    }

    // ------------------------------------------------------------- sequencer

    #[test]
    fn the_sequencer_state_serialises_with_the_keys_the_panels_read() {
        let dto = SequencerStateDto {
            bpm: 130.0,
            sample_rate: 44_100.0,
            play_mode: "pat",
            ppq: mydaw_plugin_sequencer::PPQ,
            ticks_per_step: mydaw_plugin_sequencer::TICKS_PER_STEP,
            ticks_per_bar: TICKS_PER_BAR,
            pattern_set: PatternSetDto::from(&starter()),
            playlist: PlaylistDto::from(&Playlist::default()),
        };
        let json: serde_json::Value = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["bpm"], 130.0);
        assert_eq!(json["sampleRate"], 44_100.0);
        assert_eq!(json["playMode"], "pat");
        assert_eq!(json["ppq"], 960);
        assert_eq!(json["ticksPerStep"], 240);
        assert_eq!(json["ticksPerBar"], 3_840);

        // Mirrors `SequencerState` in ui/src/api/types.ts, key for key.
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "bpm",
                "patternSet",
                "playMode",
                "playlist",
                "ppq",
                "sampleRate",
                "ticksPerBar",
                "ticksPerStep"
            ]
        );

        let set = &json["patternSet"];
        assert_eq!(set["activeId"], 1);
        assert_eq!(set["patterns"].as_array().unwrap().len(), 1);
        assert_eq!(set["patterns"][0]["name"], "Pattern 1");
        assert_eq!(set["patterns"][0]["hitCount"], 0);
        assert_eq!(set["patterns"][0]["lengthTicks"], 3_840);
        assert_eq!(set["maxPatterns"], mydaw_plugin_sequencer::MAX_PATTERNS);

        let active = &set["active"];
        assert_eq!(active["steps"], 16);
        let channels = active["channels"].as_array().unwrap();
        assert_eq!(channels.len(), 5);
        assert_eq!(channels[0]["channel"], 0);
        assert_eq!(channels[0]["mode"], "steps");
        assert_eq!(channels[0]["steps"]["key"], 36);
        assert_eq!(channels[0]["steps"]["label"], "Kick");
        assert_eq!(channels[0]["steps"]["steps"].as_array().unwrap().len(), 16);
        assert_eq!(channels[0]["steps"]["steps"][0]["on"], false);
        assert_eq!(channels[3]["steps"]["label"], "OH");
        assert!(channels[0]["notes"].as_array().unwrap().is_empty());

        let playlist = &json["playlist"];
        assert_eq!(playlist["tracks"].as_array().unwrap().len(), 4);
        assert_eq!(playlist["tracks"][0]["name"], "Track 1");
        assert_eq!(playlist["tracks"][0]["muted"], false);
        assert!(playlist["clips"].as_array().unwrap().is_empty());
        assert_eq!(playlist["endTicks"], 0);
    }

    #[test]
    fn a_colour_is_absent_rather_than_null_when_the_engine_has_none() {
        // The engine has no palette, and `"color": null` would make every
        // consumer write a null check for a field that simply is not there.
        let json = serde_json::to_string(&PatternDto::from(&Pattern::silent(1, "P"))).unwrap();
        assert!(!json.contains("color"), "{json}");

        let mut pattern = Pattern::silent(1, "P");
        pattern.color = Some("#4f8ef7".to_owned());
        let json: serde_json::Value = serde_json::to_value(PatternDto::from(&pattern)).unwrap();
        assert_eq!(json["color"], "#4f8ef7");
    }

    #[test]
    fn a_pattern_round_trips_through_the_wire_shape_unchanged() {
        let mut pattern =
            Pattern::with_channels(7, "Verse", &[0, 4], |c| if c == 0 { 36 } else { 60 });
        pattern.color = Some("#47b8a8".to_owned());
        pattern.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 4, 8, 12]));
        pattern.channels[0].steps.as_mut().unwrap().steps[4] = Step::accent();
        pattern.channels[1].steps = None;
        pattern.channels[1]
            .notes
            .push(MelodicNote::new(64, 240, 480).at_velocity(0.5));

        let dto = PatternDto::from(&pattern);
        let json = serde_json::to_string(&dto).unwrap();
        let parsed: PatternDto = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, dto);
        assert_eq!(Pattern::try_from(&parsed).unwrap(), pattern);
    }

    #[test]
    fn the_display_mode_is_derived_on_the_way_out_and_ignored_on_the_way_in() {
        let mut pattern = Pattern::with_channels(1, "P", &[0], |_| 36);
        assert_eq!(PatternDto::from(&pattern).channels[0].mode, "steps");

        pattern.channels[0].notes.push(MelodicNote::new(60, 0, 240));
        assert_eq!(
            PatternDto::from(&pattern).channels[0].mode,
            "roll",
            "a grid cannot show a note length, so notes decide"
        );

        // A mode the webview invented has no effect: the content decides.
        let mut dto = PatternDto::from(&pattern);
        dto.channels[0].mode = "whatever the webview felt like".to_owned();
        let back = Pattern::try_from(&dto).unwrap();
        assert_eq!(back.channels[0].display_mode().id(), "roll");
    }

    #[test]
    fn the_step_label_is_derived_on_the_way_out_and_ignored_on_the_way_in() {
        let json = format!(
            r#"{{
                "id": 3,
                "name": "Drums",
                "steps": 16,
                "channels": [{{
                    "channel": 0,
                    "mode": "nonsense",
                    "steps": {{
                        "key": 36,
                        "label": "whatever the webview felt like",
                        "steps": [{}]
                    }},
                    "notes": []
                }}]
            }}"#,
            std::iter::repeat_n(r#"{"on": false, "velocity": 0.78}"#, DEFAULT_STEPS)
                .collect::<Vec<_>>()
                .join(",")
        );
        let dto: PatternDto = serde_json::from_str(&json).unwrap();
        let pattern = Pattern::try_from(&dto).unwrap();
        assert_eq!(pattern.id, 3);
        assert_eq!(pattern.name, "Drums");
        assert_eq!(pattern.channels[0].steps.as_ref().unwrap().key, 36);
        // On the way back out the label is the one the drum map says.
        assert_eq!(
            PatternDto::from(&pattern).channels[0]
                .steps
                .as_ref()
                .unwrap()
                .label,
            "Kick"
        );
    }

    #[test]
    fn a_step_row_of_the_wrong_length_is_rejected_by_name_rather_than_padded() {
        let dto = PatternDto {
            id: 1,
            name: "P".to_owned(),
            color: None,
            steps: 16,
            channels: vec![ChannelClipDto {
                channel: 0,
                mode: String::new(),
                steps: Some(StepRowDto {
                    key: 36,
                    label: String::new(),
                    steps: vec![
                        StepDto {
                            on: true,
                            velocity: 1.0
                        };
                        15
                    ],
                }),
                notes: Vec::new(),
            }],
        };
        assert_eq!(
            Pattern::try_from(&dto),
            Err(PatternError::WrongRowLength {
                clip: 0,
                got: 15,
                expected: 16
            })
        );
    }

    #[test]
    fn a_pattern_the_engine_cannot_run_is_refused_at_the_boundary() {
        let clip = |channel: u8, key: u8, velocity: f32| ChannelClipDto {
            channel,
            mode: String::new(),
            steps: Some(StepRowDto {
                key,
                label: String::new(),
                steps: vec![StepDto { on: true, velocity }; DEFAULT_STEPS],
            }),
            notes: Vec::new(),
        };
        let pattern = |channels: Vec<ChannelClipDto>, steps: u8| PatternDto {
            id: 1,
            name: "P".to_owned(),
            color: None,
            steps,
            channels,
        };

        // Too many channel clips.
        assert!(matches!(
            Pattern::try_from(&pattern(
                (0..17).map(|c| clip(c as u8, 36, 1.0)).collect(),
                16
            )),
            Err(PatternError::TooManyChannels(17))
        ));

        // The wrong number of steps.
        assert!(matches!(
            Pattern::try_from(&pattern(vec![clip(0, 36, 1.0)], 8)),
            Err(PatternError::WrongStepCount(8))
        ));

        // Two clips on one channel.
        assert!(matches!(
            Pattern::try_from(&pattern(vec![clip(0, 36, 1.0), clip(0, 38, 1.0)], 16)),
            Err(PatternError::DuplicateChannel { channel: 0 })
        ));

        // A velocity the webview should never have sent.
        for bad in [-0.5_f32, 1.5, f32::NAN] {
            assert!(
                matches!(
                    Pattern::try_from(&pattern(vec![clip(0, 36, bad)], 16)),
                    Err(PatternError::InvalidVelocity { .. })
                ),
                "velocity {bad} should have been refused"
            );
        }

        // A key outside the MIDI range.
        assert!(matches!(
            Pattern::try_from(&pattern(vec![clip(0, 200, 1.0)], 16)),
            Err(PatternError::KeyOutOfRange {
                channel: 0,
                key: 200
            })
        ));
    }

    #[test]
    fn a_step_with_no_velocity_at_all_gets_the_default_rather_than_zero() {
        let dto: StepDto = serde_json::from_str(r#"{"on": true}"#).unwrap();
        assert!(dto.on);
        assert_eq!(dto.velocity, DEFAULT_STEP_VELOCITY);
    }

    #[test]
    fn a_clip_carries_only_the_fields_its_kind_has() {
        let mut playlist = Playlist::default();
        let pattern = playlist.add_pattern_clip(1, 0, 0, 4 * BAR).unwrap();
        let audio = playlist
            .add_audio_clip(9, 2, 8 * BAR, 4 * BAR, 12_000, -3.0)
            .unwrap();

        let json: serde_json::Value = serde_json::to_value(PlaylistDto::from(&playlist)).unwrap();
        let clips = json["clips"].as_array().unwrap();

        let pattern_clip = clips.iter().find(|c| c["id"] == pattern).unwrap();
        assert_eq!(pattern_clip["kind"], "pattern");
        assert_eq!(pattern_clip["patternId"], 1);
        assert_eq!(pattern_clip["start"], 0);
        assert_eq!(pattern_clip["length"], 4 * BAR);
        assert!(
            pattern_clip.get("sourceId").is_none() && pattern_clip.get("gainDb").is_none(),
            "a pattern clip has no source and no gain of its own: {pattern_clip}"
        );

        let audio_clip = clips.iter().find(|c| c["id"] == audio).unwrap();
        assert_eq!(audio_clip["kind"], "audio");
        assert_eq!(audio_clip["sourceId"], 9);
        assert_eq!(audio_clip["offsetFrames"], 12_000);
        assert_eq!(audio_clip["gainDb"], -3.0);
        assert!(
            audio_clip.get("patternId").is_none(),
            "an audio clip plays no pattern: {audio_clip}"
        );

        assert_eq!(json["endTicks"], 12 * BAR);
        assert_eq!(json["maxClips"], mydaw_plugin_sequencer::MAX_CLIPS);
    }

    #[test]
    fn the_summary_counts_what_a_picker_row_shows_without_sending_the_pattern() {
        let mut pattern = Pattern::with_channels(4, "Drums", &[0, 1], |_| 36);
        pattern.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 4, 8, 12]));
        pattern.channels[1].notes.push(MelodicNote::new(60, 0, 240));

        let summary = PatternSummaryDto::from(&pattern);
        assert_eq!(summary.id, 4);
        assert_eq!(summary.name, "Drums");
        assert_eq!(summary.steps, 16);
        assert_eq!(summary.length_ticks, 3_840);
        assert_eq!(summary.channel_count, 2);
        assert_eq!(summary.hit_count, 4);
        assert_eq!(summary.note_count, 1);
    }
}
