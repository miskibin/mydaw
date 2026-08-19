//! The project: what tracks there are, and what each one is set to.
//!
//! This is the **non-RT, editable** side of the mixer, in the same relationship
//! to [`MixerParams`](mydaw_rt_core::MixerParams) that
//! [`Pattern`](mydaw_plugin_sequencer::Pattern) has to `CompiledPattern`: the
//! shapes here are what a project file will hold and what the UI edits, and the
//! audio thread never sees them. What crosses the wall is a graph compiled from
//! this, plus a block of atomics the strips read.
//!
//! ```text
//!   webview            app (non-RT, here)                 audio thread
//!   -------            ------------------                 ------------
//!   set_track_gain --> TrackDef.gain_db --> MixerParams --> TrackStrip
//!   set_track_solo --> solo set --> effective mutes ----->  (mute is a ramp)
//!   add_track ------>  Project.tracks --> GraphSpec -----> CompiledGraph
//! ```
//!
//! # Why gain is stored in decibels and published in linear
//! A fader is a dB control -- that is the scale it is drawn on, the scale a user
//! thinks in, and the scale a project file should record, because -6.0 dB is
//! exact and 0.5011872 is a rounding of it. The audio thread wants a multiplier
//! and must not call `powf` per block, so the conversion happens exactly once,
//! here, on the way in.
//!
//! # Why solo is not a thing the audio thread knows about
//! "Is this track audible" is a question about the *whole project*: track 2 is
//! silent because track 0 is soloed, which the strip for track 2 cannot see
//! without reading every other track's state every block. So solo is resolved
//! here into an **effective mute** per track, and the audio thread only ever
//! reads one bool. Soloing is then exactly as click-free as muting, because it
//! *is* muting.

use mydaw_rt_core::{db_to_gain, gain_to_db, MixerParams, MAX_GAIN_DB, MAX_TRACKS, SILENCE_DB};
use serde::Serialize;

use crate::source::Source;

/// One track, as the session holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackDef {
    /// Stable across renumbering, so the UI can key a list by it. Not the note
    /// channel -- see [`Project`].
    pub id: u32,
    pub name: String,
    /// What sits at the head of this track's chain.
    pub source: Source,
    /// Fader position, in dB. [`SILENCE_DB`] is off.
    pub gain_db: f32,
    /// Pan, `-1.0` hard left to `1.0` hard right.
    pub pan: f32,
    pub muted: bool,
    pub soloed: bool,
}

impl TrackDef {
    /// A track at unity gain, centred, unmuted.
    pub fn new(id: u32, name: impl Into<String>, source: Source) -> Self {
        Self {
            id,
            name: name.into(),
            source,
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
        }
    }

    /// The linear multiplier this track's fader asks for.
    pub fn gain(&self) -> f32 {
        db_to_gain(self.gain_db)
    }
}

/// Why a track edit was refused.
///
/// Every variant is reachable from the webview, which can send any index it
/// likes, plus one that is a genuine engine limitation.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TrackError {
    #[error("there is no track {0}")]
    NoSuchTrack(usize),
    #[error("a project may have at most {MAX_TRACKS} tracks")]
    TooManyTracks,
    #[error("a project must keep at least one track")]
    LastTrack,
    #[error("gain {0} dB is not a number between {SILENCE_DB} and {MAX_GAIN_DB}")]
    InvalidGain(f32),
    #[error("pan {0} is not a number between -1.0 and 1.0")]
    InvalidPan(f32),
    /// Adding or removing a track rebuilds the graph, which destroys and
    /// recreates every node in it -- and a hosted CLAP plugin can only be
    /// activated once, from the main thread. See the step 5 spec.
    #[error(
        "adding or removing a track is not supported while a CLAP plugin is in the graph; \
         restart MYDAW without MYDAW_CLAP_INSTRUMENT / MYDAW_CLAP_FX to change the track layout"
    )]
    ClapInGraph,
}

/// The session's tracks and master fader.
///
/// # Track index is note channel
/// A track's **index** in [`tracks`](Self::tracks) is the note channel it
/// answers to: a sequencer row on channel 1 plays track 1, and the keyboard
/// piano sends on the selected track's index. That is why `id` exists as a
/// separate field -- removing a track renumbers everything after it, and a UI
/// list keyed on position would animate wrongly. It also means removing a track
/// re-points any sequencer row addressed at a later one, which is a known
/// sharp edge of this step rather than a design.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub tracks: Vec<TrackDef>,
    /// Master fader, in dB.
    pub master_gain_db: f32,
    next_id: u32,
}

impl Default for Project {
    /// Drums on track 0, a synth on track 1.
    ///
    /// Two tracks rather than one because the point of the step is that they
    /// sum, and rather than four because a mixer full of silent tracks is a
    /// worse first impression than one you have to add to. Drums first so the
    /// sequencer's four default rows -- which are on channel 0, and are a kick,
    /// a snare and two hats -- address the kit that can play them.
    fn default() -> Self {
        Self {
            tracks: vec![
                TrackDef::new(0, "Drums", Source::DrumKit),
                TrackDef::new(1, "Synth", Source::PolySynth),
            ],
            master_gain_db: 0.0,
            next_id: 2,
        }
    }
}

impl Project {
    /// A project with no tracks at all. Renders silence.
    pub fn empty() -> Self {
        Self {
            tracks: Vec::new(),
            master_gain_db: 0.0,
            next_id: 0,
        }
    }

    /// Tracks in this project.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Whether any track is soloed.
    pub fn any_soloed(&self) -> bool {
        self.tracks.iter().any(|track| track.soloed)
    }

    /// Whether `index` is silent right now, solo included.
    ///
    /// This is the whole of the solo rule: with nothing soloed a track is
    /// audible unless it is muted; with anything soloed a track is audible only
    /// if it is itself soloed. A soloed *and* muted track stays audible, which
    /// is what every DAW does -- solo is an override, not a vote.
    pub fn effective_mute(&self, index: usize) -> bool {
        let Some(track) = self.tracks.get(index) else {
            return true;
        };
        if self.any_soloed() {
            !track.soloed
        } else {
            track.muted
        }
    }

    /// Append a track named after its source, returning its index.
    ///
    /// # Errors
    /// [`TrackError::TooManyTracks`] past [`MAX_TRACKS`].
    pub fn add(&mut self, source: Source, name: Option<String>) -> Result<usize, TrackError> {
        if self.tracks.len() >= MAX_TRACKS {
            return Err(TrackError::TooManyTracks);
        }
        let id = self.next_id;
        self.next_id += 1;
        let name = name.unwrap_or_else(|| source.label().to_owned());
        self.tracks.push(TrackDef::new(id, name, source));
        Ok(self.tracks.len() - 1)
    }

    /// Remove track `index`, renumbering the ones after it.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], or [`TrackError::LastTrack`] -- a project
    /// with no tracks has no keyboard target and no meters, and "delete
    /// everything" is not a mixer gesture.
    pub fn remove(&mut self, index: usize) -> Result<TrackDef, TrackError> {
        if index >= self.tracks.len() {
            return Err(TrackError::NoSuchTrack(index));
        }
        if self.tracks.len() == 1 {
            return Err(TrackError::LastTrack);
        }
        Ok(self.tracks.remove(index))
    }

    /// Whether any track's source is the hosted CLAP instrument.
    pub fn has_clap_source(&self) -> bool {
        self.tracks.iter().any(|track| track.source == Source::Clap)
    }

    /// Publish every track's gain, pan and effective mute, plus the master
    /// fader, into the block the audio thread reads.
    ///
    /// Idempotent and cheap (three relaxed stores per track), so callers push
    /// the whole project rather than tracking which field changed.
    pub fn publish(&self, params: &MixerParams) {
        for index in 0..MAX_TRACKS {
            match self.tracks.get(index) {
                Some(track) => {
                    params.set_track_gain(index, track.gain());
                    params.set_track_pan(index, track.pan);
                    params.set_track_muted(index, self.effective_mute(index));
                }
                // A slot with no track behind it is silent, so a stale graph
                // that still has one cannot make a sound.
                None => params.set_track_muted(index, true),
            }
        }
        params.set_master_gain(db_to_gain(self.master_gain_db));
    }
}

/// Validate a fader value the webview sent.
///
/// # Errors
/// [`TrackError::InvalidGain`] for NaN, infinity or anything outside the
/// fader's own scale -- rejected rather than clamped, so a UI bug is a status
/// bar sentence instead of a silently different mix.
pub fn check_gain_db(db: f32) -> Result<f32, TrackError> {
    if !db.is_finite() || !(SILENCE_DB..=MAX_GAIN_DB).contains(&db) {
        return Err(TrackError::InvalidGain(db));
    }
    Ok(db)
}

/// Validate a pan value the webview sent.
///
/// # Errors
/// [`TrackError::InvalidPan`].
pub fn check_pan(pan: f32) -> Result<f32, TrackError> {
    if !pan.is_finite() || !(-1.0..=1.0).contains(&pan) {
        return Err(TrackError::InvalidPan(pan));
    }
    Ok(pan)
}

// -------------------------------------------------------------------- the wire

/// One mixer strip, as the webview draws it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackDto {
    /// Position, which is also this track's note channel.
    pub index: usize,
    /// Stable identity across renumbering. Key a UI list on this.
    pub id: u32,
    pub name: String,
    /// Source id, as [`get_sources`](crate::commands::get_sources) reports them.
    pub source_id: String,
    pub source_label: String,
    pub gain_db: f32,
    pub pan: f32,
    /// The button state, not the outcome. See [`silenced`](Self::silenced).
    pub muted: bool,
    pub soloed: bool,
    /// Whether this track is actually silent right now, solo resolved.
    ///
    /// Sent so the mixer can grey a track that another track's solo has
    /// silenced, which is the difference between a mute button and a mute.
    pub silenced: bool,
    /// Whether the keyboard piano and the source selector address this track.
    pub selected: bool,
}

/// The master strip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterDto {
    pub gain_db: f32,
}

/// Everything the mixer panel needs in one message.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MixerStateDto {
    pub tracks: Vec<TrackDto>,
    pub master: MasterDto,
    /// Index of the track the keyboard and the source selector address.
    pub selected: usize,
    /// The engine's fixed track limit, so the UI can grey out "add".
    pub max_tracks: usize,
    /// Fader bounds, so the UI does not keep its own copy of the scale.
    pub min_gain_db: f32,
    pub max_gain_db: f32,
    /// Whether the track layout can be edited at all. False when a CLAP plugin
    /// is in the graph; see [`TrackError::ClapInGraph`].
    pub can_edit_layout: bool,
}

/// Render `project` for the webview.
pub fn mixer_state_dto(project: &Project, selected: usize, can_edit_layout: bool) -> MixerStateDto {
    MixerStateDto {
        tracks: project
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| TrackDto {
                index,
                id: track.id,
                name: track.name.clone(),
                source_id: track.source.id().to_owned(),
                source_label: track.source.label().to_owned(),
                gain_db: track.gain_db,
                pan: track.pan,
                muted: track.muted,
                soloed: track.soloed,
                silenced: project.effective_mute(index),
                selected: index == selected,
            })
            .collect(),
        master: MasterDto {
            gain_db: project.master_gain_db,
        },
        selected,
        max_tracks: MAX_TRACKS,
        min_gain_db: SILENCE_DB,
        max_gain_db: MAX_GAIN_DB,
        can_edit_layout,
    }
}

/// The dB a linear multiplier corresponds to. Re-exported for the UI's readouts
/// and for tests that measure gain rather than set it.
pub fn linear_to_db(gain: f32) -> f32 {
    gain_to_db(gain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_project_is_drums_on_track_zero_and_a_synth_on_track_one() {
        let project = Project::default();
        assert_eq!(project.len(), 2);
        assert_eq!(project.tracks[0].name, "Drums");
        assert_eq!(project.tracks[0].source, Source::DrumKit);
        assert_eq!(project.tracks[1].name, "Synth");
        assert_eq!(project.tracks[1].source, Source::PolySynth);
        assert!(project
            .tracks
            .iter()
            .all(|t| t.gain_db == 0.0 && t.pan == 0.0 && !t.muted && !t.soloed));
        assert_eq!(project.master_gain_db, 0.0);

        // Track 0 is the kit because the sequencer's four default rows are on
        // channel 0 and are a kick, a snare and two hats.
        assert_eq!(
            mydaw_plugin_sequencer::Pattern::default()
                .rows
                .iter()
                .map(|r| r.channel)
                .collect::<Vec<_>>(),
            vec![0, 0, 0, 0]
        );
    }

    #[test]
    fn a_fader_in_decibels_becomes_the_multiplier_the_strip_wants() {
        let mut track = TrackDef::new(0, "T", Source::PolySynth);
        assert_eq!(track.gain(), 1.0);
        track.gain_db = -6.0206;
        assert!((track.gain() - 0.5).abs() < 1e-4);
        track.gain_db = SILENCE_DB;
        assert_eq!(track.gain(), 0.0, "the bottom of the fader is silence");
        assert!((linear_to_db(1.0)).abs() < 1e-6);
    }

    #[test]
    fn solo_silences_everything_that_is_not_soloed_and_un_solo_restores() {
        let mut project = Project::default();
        project.add(Source::TestTone, None).unwrap();
        assert_eq!(project.len(), 3);
        assert!(!project.any_soloed());
        assert!((0..3).all(|i| !project.effective_mute(i)));

        project.tracks[1].soloed = true;
        assert!(project.any_soloed());
        assert!(project.effective_mute(0));
        assert!(!project.effective_mute(1));
        assert!(project.effective_mute(2));

        // A second solo adds to the set rather than replacing it.
        project.tracks[2].soloed = true;
        assert!(!project.effective_mute(2));
        assert!(project.effective_mute(0));

        project.tracks[1].soloed = false;
        project.tracks[2].soloed = false;
        assert!((0..3).all(|i| !project.effective_mute(i)), "restored");
    }

    #[test]
    fn a_soloed_track_is_audible_even_if_it_is_also_muted() {
        // Solo is an override, not a vote: every DAW does this, and the
        // alternative is a solo button that sometimes does nothing.
        let mut project = Project::default();
        project.tracks[0].muted = true;
        project.tracks[0].soloed = true;
        assert!(!project.effective_mute(0));
        assert!(project.effective_mute(1));
    }

    #[test]
    fn a_mute_only_matters_while_nothing_is_soloed() {
        let mut project = Project::default();
        project.tracks[0].muted = true;
        assert!(project.effective_mute(0));
        assert!(!project.effective_mute(1));

        project.tracks[1].soloed = true;
        assert!(project.effective_mute(0), "still silent, for a new reason");
        assert!(!project.effective_mute(1));
    }

    #[test]
    fn publishing_pushes_every_track_and_mutes_the_slots_that_have_none() {
        let mut project = Project::default();
        project.tracks[0].gain_db = -6.0206;
        project.tracks[1].pan = -1.0;
        project.tracks[1].muted = true;
        project.master_gain_db = -12.0;

        let params = MixerParams::new();
        project.publish(&params);

        assert!((params.track_gain(0) - 0.5).abs() < 1e-4);
        assert!(!params.track_muted(0));
        assert_eq!(params.track_pan(1), -1.0);
        assert!(params.track_muted(1));
        assert!((params.master_gain() - 0.251_188_6).abs() < 1e-5);

        // A slot with no track behind it must be silent, so a graph that still
        // has one cannot make a sound.
        assert!(params.track_muted(2));
        assert!(params.track_muted(MAX_TRACKS - 1));
    }

    #[test]
    fn tracks_are_added_up_to_the_engines_limit_and_never_past_it() {
        let mut project = Project::default();
        while project.len() < MAX_TRACKS {
            project.add(Source::PolySynth, None).unwrap();
        }
        assert_eq!(
            project.add(Source::PolySynth, None),
            Err(TrackError::TooManyTracks)
        );
        assert_eq!(project.len(), MAX_TRACKS);
    }

    #[test]
    fn removing_a_track_renumbers_the_ones_after_it_but_keeps_their_ids() {
        let mut project = Project::default();
        project
            .add(Source::TestTone, Some("Tone".to_owned()))
            .unwrap();
        let ids: Vec<u32> = project.tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![0, 1, 2]);

        let removed = project.remove(0).unwrap();
        assert_eq!(removed.name, "Drums");
        assert_eq!(
            project.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![1, 2],
            "ids are stable; only the index -- and therefore the note channel -- moved"
        );

        assert_eq!(project.remove(5), Err(TrackError::NoSuchTrack(5)));
        project.remove(0).unwrap();
        assert_eq!(project.remove(0), Err(TrackError::LastTrack));
    }

    #[test]
    fn a_new_track_is_named_after_its_source_unless_told_otherwise() {
        let mut project = Project::empty();
        let index = project.add(Source::DrumKit, None).unwrap();
        assert_eq!(project.tracks[index].name, "Drum Kit");
        let index = project
            .add(Source::TestTone, Some("Ref".to_owned()))
            .unwrap();
        assert_eq!(project.tracks[index].name, "Ref");
        assert!(!project.has_clap_source());
        project.tracks[0].source = Source::Clap;
        assert!(project.has_clap_source());
    }

    #[test]
    fn values_the_webview_should_never_have_sent_are_named_rather_than_clamped() {
        assert_eq!(check_gain_db(0.0), Ok(0.0));
        assert_eq!(check_gain_db(SILENCE_DB), Ok(SILENCE_DB));
        assert_eq!(check_gain_db(MAX_GAIN_DB), Ok(MAX_GAIN_DB));
        for bad in [f32::NAN, f32::INFINITY, -61.0, 7.0] {
            assert!(
                matches!(check_gain_db(bad), Err(TrackError::InvalidGain(_))),
                "{bad}"
            );
        }

        assert_eq!(check_pan(-1.0), Ok(-1.0));
        assert_eq!(check_pan(1.0), Ok(1.0));
        for bad in [f32::NAN, 1.5, -2.0] {
            assert!(
                matches!(check_pan(bad), Err(TrackError::InvalidPan(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_wire_shape_carries_what_the_mixer_panel_draws() {
        let mut project = Project::default();
        project.tracks[0].soloed = true;
        let dto = mixer_state_dto(&project, 1, true);

        assert_eq!(dto.tracks.len(), 2);
        assert_eq!(dto.selected, 1);
        assert_eq!(dto.max_tracks, MAX_TRACKS);
        assert_eq!(dto.min_gain_db, SILENCE_DB);
        assert!(dto.can_edit_layout);
        assert_eq!(dto.tracks[0].index, 0);
        assert_eq!(dto.tracks[0].source_id, "drum-kit");
        assert!(dto.tracks[0].soloed && !dto.tracks[0].silenced);
        assert!(
            dto.tracks[1].silenced && !dto.tracks[1].muted,
            "silenced by somebody else's solo, with its own button still off"
        );
        assert!(dto.tracks[1].selected && !dto.tracks[0].selected);

        // Mirrors `MixerState` in ui/src/api/types.ts, key for key.
        let json: serde_json::Value = serde_json::to_value(&dto).unwrap();
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
                "canEditLayout",
                "master",
                "maxGainDb",
                "minGainDb",
                "maxTracks",
                "selected",
                "tracks"
            ]
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
        let mut track_keys: Vec<&str> = json["tracks"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        track_keys.sort_unstable();
        assert_eq!(
            track_keys,
            [
                "gainDb",
                "id",
                "index",
                "muted",
                "name",
                "pan",
                "selected",
                "silenced",
                "soloed",
                "sourceId",
                "sourceLabel"
            ]
        );
        assert_eq!(json["master"]["gainDb"], 0.0);
    }
}
