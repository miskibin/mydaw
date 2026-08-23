//! `mydaw-plugin-sequencer` -- patterns, the arrangement, and the clock that
//! walks them.
//!
//! Step 3 gave the engine instruments and a computer-keyboard piano. That makes
//! sounds, but a DAW is not a synthesiser: it is a machine for laying sounds out
//! *in time*. This crate is that machine.
//!
//! | Module | What |
//! | --- | --- |
//! | [`pattern`] | [`Pattern`] / [`ChannelClip`] / [`StepRow`] / [`MelodicNote`]: one pattern, its bounds, and its typed rejections. |
//! | [`set`] | [`PatternSet`]: every pattern in the project, addressed by stable id, and which one is being edited. |
//! | [`playlist`] | [`Playlist`]: lanes and clips -- patterns and audio placed along the song. |
//! | [`compiled`] | [`CompiledSong`]: the whole thing in the shape the audio thread reads, plus the `basedrop` channel that carries it across the RT wall. |
//! | [`schedule`] | [`Scheduler`]: song + transport window -> note events, sample-accurate and drift-free, in [`PlayMode::Pattern`] or [`PlayMode::Song`]. |
//!
//! # The three ideas worth reading
//!
//! **Everything is notes.** A step is an editing shorthand;
//! [`Pattern::compile`] expands it into a note of [`STEP_GATE_TICKS`], and the
//! audio thread has one kind of event and one loop. See `pattern`'s module docs
//! for why that trade goes this way.
//!
//! **Nothing accumulates.** A position is `round(tick / 240 * step_len)` from
//! the **absolute** tick, so there is no running total to drift, no phase to
//! reset at a loop point, and no state to get wrong after a seek. A pattern clip
//! four hundred bars into a song is the same arithmetic as the first bar of the
//! first pattern.
//!
//! **Ids, not indices.** A pattern has an id that outlives its position, because
//! a playlist clip references it and reordering the picker must not silently
//! rewrite the arrangement.
//!
//! See `docs/superpowers/specs/2026-08-19-engine-patterns-playlist.md`.
//!
//! # Using it
//! ```no_run
//! use mydaw_kernel::Kernel;
//! use mydaw_plugin_sequencer::{SequencerPlugin, SequencerService};
//!
//! let mut kernel = Kernel::new();
//! kernel.add_plugin(Box::new(SequencerPlugin::new())).unwrap();
//! kernel.boot().unwrap();
//!
//! let sequencer = kernel.ctx().services.get::<SequencerService>().unwrap();
//! let song = sequencer.compile(); // hand this to the audio thread
//! ```

pub mod audio;
pub mod clips;
pub mod compiled;
pub mod oneshot;
pub mod pattern;
pub mod playlist;
pub mod sample_channel;
pub mod schedule;
pub mod set;

pub use audio::{
    reference_count, AudioSource, AudioSourceError, AudioSourceRegistry, WaveformMip, MAX_SOURCES,
    MAX_SOURCE_SECONDS, MAX_WAVEFORM_BINS, MIP_BASE_FRAMES, MIP_LEVELS, MIP_RATIO,
};
pub use clips::{
    audio_clip_channel, compile_audio_clips, compile_audio_clips_with, pitch_ratio, playback_step,
    ramp_step, AudioClipHandle, AudioClipInstallError, AudioClipNode, AudioClipStats,
    AudioClipTune, CompiledAudioClip, CompiledAudioSong, AUDIO_CLIP_QUEUE_CAPACITY,
    AUDIO_CLIP_VOICES, CLIP_RAMP_MS,
};
pub use compiled::{
    song_channel, CompiledNote, CompiledPattern, CompiledPatternClip, CompiledSong, SongHandle,
    SongInstallError, SongSlot, SONG_QUEUE_CAPACITY,
};
/// The file extensions a "load audio" dialog should offer, from the decoder
/// crate. Re-exported so the shell can reach it without depending on
/// `mydaw-audio-file` directly for one constant.
pub use mydaw_audio_file::AUDIO_EXTENSIONS;
pub use oneshot::{
    OneShotNode, SamplePlaybackParams, SliceTable, MAX_SLICES, ONESHOT_ROOT_KEY, ONESHOT_VOICES,
    SLICE_DECLICK_MS,
};
pub use pattern::{
    row_label, ChannelClip, ChannelDisplay, MelodicNote, Pattern, PatternError, Step, StepRow,
    ACCENT_THRESHOLD, ACCENT_VELOCITY, DEFAULT_ROW_KEYS, DEFAULT_STEPS, DEFAULT_STEP_KEY,
    DEFAULT_STEP_VELOCITY, MAX_CHANNEL, MAX_NOTES, MAX_PATTERN_CHANNELS, MAX_STEPS,
    PATTERN_LENGTHS, PPQ, STEPS_PER_BEAT, STEP_GATE_TICKS, TICKS_PER_BAR, TICKS_PER_STEP,
};
pub use playlist::{
    AudioClip, Clip, PatternClip, Playlist, PlaylistError, PlaylistTrack, DEFAULT_CLIP_TICKS,
    MAX_CLIPS, MAX_PLAYLIST_TRACKS, MAX_SONG_TICKS,
};
pub use sample_channel::SampleChannelHead;
pub use schedule::{
    step_boundary, step_len_samples, tick_boundary, ticks_at_sample, PlayMode, Scheduler,
    TransportWindow, MAX_PENDING,
};
pub use set::{PatternSet, PatternSetError, MAX_PATTERNS, NO_PATTERN};

use std::sync::Arc;

use mydaw_kernel::{KernelCtx, PanelManifest, Plugin, PluginManifest};
use mydaw_rt_core::{clamp_bpm, DEFAULT_BPM};
use parking_lot::Mutex;

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "sequencer";

/// Display name in the plugin list.
pub const PLUGIN_NAME: &str = "Sequencer";

/// Panel id and frontend component key of the channel rack / step grid.
/// Must match the key in `ui/src/panels/registry.ts`.
pub const SEQUENCER_PANEL: &str = "step-sequencer";

/// Panel id and frontend component key of the piano roll.
///
/// Contributed by this plugin rather than by the shell, because the pattern the
/// roll edits is this plugin's: a panel is offered by whoever owns the state it
/// draws.
pub const PIANO_ROLL_PANEL: &str = "piano-roll";

/// Panel id and frontend component key of the playlist.
pub const PLAYLIST_PANEL: &str = "playlist";

/// The authoritative, non-RT copy of the song: patterns, arrangement, tempo and
/// play mode.
///
/// # Why the tempo and the mode live here as well as on the clock
/// [`TransportClock`](mydaw_rt_core::TransportClock) is built around a device
/// sample rate and therefore only exists while a stream is open. A user who
/// changes the tempo on a machine with no sound card, or before the device has
/// come up, has still changed the tempo -- and the value has to survive until
/// there is a clock to publish it to. So this service owns what the *session* is
/// set to, and the shell pushes it to each new stream.
///
/// The same reasoning applies to the patterns and the playlist, which are
/// project data and get saved to a file; the compiled copy on the audio thread
/// is a projection of them, never the original.
///
/// One `Mutex` over the whole thing rather than one per field: an edit is a
/// webview command at human speed, several of them are "read, modify, write" over
/// two of these values at once (deleting a pattern touches the set *and* the
/// playlist), and a lock per field would be an invitation to publish a song that
/// was half of one edit and half of another. Never locked from the audio thread.
pub struct SequencerService {
    state: Mutex<SongState>,
}

/// Everything the service owns, behind one lock.
#[derive(Debug, Clone, PartialEq)]
struct SongState {
    patterns: PatternSet,
    playlist: Playlist,
    bpm: f64,
    mode: PlayMode,
    /// Whether song mode repeats the arrangement. Mirrors the transport's own
    /// flag so the shell can answer without a device open, exactly as `mode`
    /// does.
    looping: bool,
}

impl Default for SequencerService {
    fn default() -> Self {
        Self::new()
    }
}

impl SequencerService {
    /// One empty pattern, an empty four-lane playlist, [`DEFAULT_BPM`], pattern
    /// mode.
    pub fn new() -> Self {
        Self::with_song(PatternSet::default(), Playlist::default())
    }

    /// A service on an explicit set and playlist (the shell's startup path, a
    /// project load, and the tests).
    pub fn with_song(patterns: PatternSet, playlist: Playlist) -> Self {
        Self {
            state: Mutex::new(SongState {
                patterns,
                playlist,
                bpm: DEFAULT_BPM,
                mode: PlayMode::default(),
                looping: mydaw_rt_core::DEFAULT_LOOPING,
            }),
        }
    }

    /// A copy of the pattern set.
    pub fn patterns(&self) -> PatternSet {
        self.state.lock().patterns.clone()
    }

    /// A copy of the arrangement.
    pub fn playlist(&self) -> Playlist {
        self.state.lock().playlist.clone()
    }

    /// A copy of the pattern being edited.
    pub fn active_pattern(&self) -> Pattern {
        self.state.lock().patterns.active().clone()
    }

    /// Replace the whole song. Used by a project load.
    ///
    /// # Errors
    /// [`PatternError`] or [`PlaylistError`] when what was read back does not
    /// fit the engine's bounds, in which case nothing is changed -- a project
    /// file that is half-loaded is worse than one that is refused.
    pub fn set_song(&self, patterns: PatternSet, playlist: Playlist) -> Result<(), SongError> {
        patterns.validate()?;
        playlist.validate(&patterns)?;
        let mut state = self.state.lock();
        state.patterns = patterns;
        state.playlist = playlist;
        Ok(())
    }

    /// Read the set and the playlist together, under one lock.
    ///
    /// Anything that needs both -- a project save, the whole-state DTO -- has to
    /// see one consistent pair rather than two snapshots taken either side of an
    /// edit.
    pub fn song(&self) -> (PatternSet, Playlist) {
        let state = self.state.lock();
        (state.patterns.clone(), state.playlist.clone())
    }

    /// Edit the pattern set, and republish.
    ///
    /// The set is validated **after** `edit` and rolled back if it fails, so a
    /// rejected edit cannot leave the session half-changed. Every pattern
    /// command goes through here for exactly that reason.
    ///
    /// # Errors
    /// Whatever `edit` returned, or a [`PatternError`] from the result.
    pub fn edit_patterns<T>(
        &self,
        edit: impl FnOnce(&mut PatternSet) -> Result<T, PatternSetError>,
    ) -> Result<T, SongError> {
        let mut state = self.state.lock();
        let snapshot = state.patterns.clone();
        match edit(&mut state.patterns).map_err(SongError::from) {
            Ok(value) => match state.patterns.validate() {
                Ok(()) => Ok(value),
                Err(err) => {
                    state.patterns = snapshot;
                    Err(err.into())
                }
            },
            Err(err) => {
                state.patterns = snapshot;
                Err(err)
            }
        }
    }

    /// Edit the arrangement, rolling back a change that does not validate.
    ///
    /// # Errors
    /// Whatever `edit` returned, or a [`PlaylistError`] from the result.
    pub fn edit_playlist<T>(
        &self,
        edit: impl FnOnce(&mut Playlist) -> Result<T, PlaylistError>,
    ) -> Result<T, SongError> {
        let mut state = self.state.lock();
        let snapshot = state.playlist.clone();
        match edit(&mut state.playlist).map_err(SongError::from) {
            Ok(value) => match state.playlist.validate(&state.patterns) {
                Ok(()) => Ok(value),
                Err(err) => {
                    state.playlist = snapshot;
                    Err(err.into())
                }
            },
            Err(err) => {
                state.playlist = snapshot;
                Err(err)
            }
        }
    }

    /// Delete a pattern, and every clip that played it.
    ///
    /// One call rather than two because the two must not be separable: a set
    /// without the pattern and a playlist still naming it is a state no caller
    /// should be able to publish. Returns how many clips went with it, so the
    /// UI can say so.
    ///
    /// # Errors
    /// [`PatternSetError::LastPattern`] or [`PatternSetError::NoSuchPattern`].
    pub fn remove_pattern(&self, id: u64) -> Result<usize, SongError> {
        let mut state = self.state.lock();
        state.patterns.remove(id)?;
        Ok(state.playlist.drop_pattern(id))
    }

    /// Split `id` into one pattern per occupied channel and spread playlist
    /// clips onto consecutive lanes, adding lanes when they are missing.
    ///
    /// One lock, one rollback: a refused lane or clip budget restores the
    /// patterns as well, so the picker never shows voices the arrangement
    /// could not place.
    ///
    /// # Errors
    /// [`PatternSetError::NoSuchPattern`], [`PatternSetError::TooFewChannels`],
    /// [`PatternSetError::TooManyPatterns`], [`PlaylistError::TooManyTracks`]
    /// or [`PlaylistError::TooManyClips`].
    pub fn split_pattern_by_channel(
        &self,
        id: u64,
        name_for: impl Fn(u8) -> String,
    ) -> Result<Vec<u64>, SongError> {
        let mut state = self.state.lock();
        let patterns_snap = state.patterns.clone();
        let playlist_snap = state.playlist.clone();
        let result = (|| {
            let ids = state.patterns.split_by_channel(id, &name_for)?;
            state.playlist.spread_split_clips(id, &ids)?;
            state.patterns.validate()?;
            state.playlist.validate(&state.patterns)?;
            Ok(ids)
        })();
        match result {
            Ok(ids) => Ok(ids),
            Err(err) => {
                state.patterns = patterns_snap;
                state.playlist = playlist_snap;
                Err(err)
            }
        }
    }

    /// Give every pattern a step row on a newly added rack channel.
    pub fn add_channel(&self, channel: u8, key: u8) {
        self.state.lock().patterns.add_channel(channel, key);
    }

    /// Drop a removed rack channel from every pattern, shifting the ones above.
    pub fn remove_channel(&self, channel: u8) {
        self.state.lock().patterns.retarget_after_removing(channel);
    }

    /// The session tempo.
    pub fn bpm(&self) -> f64 {
        self.state.lock().bpm
    }

    /// Set the session tempo, clamped to
    /// [`MIN_BPM`](mydaw_rt_core::MIN_BPM)`..=`[`MAX_BPM`](mydaw_rt_core::MAX_BPM).
    /// Returns what was stored.
    pub fn set_bpm(&self, bpm: f64) -> f64 {
        let clamped = clamp_bpm(bpm);
        self.state.lock().bpm = clamped;
        clamped
    }

    /// Whether the transport is playing the active pattern or the arrangement.
    pub fn play_mode(&self) -> PlayMode {
        self.state.lock().mode
    }

    /// Set the play mode.
    pub fn set_play_mode(&self, mode: PlayMode) {
        self.state.lock().mode = mode;
    }

    /// Whether song mode repeats the arrangement rather than stopping at it.
    pub fn is_looping(&self) -> bool {
        self.state.lock().looping
    }

    /// Turn the song loop on or off.
    pub fn set_looping(&self, looping: bool) {
        self.state.lock().looping = looping;
    }

    /// The whole song, compiled for the audio thread. **Non-RT.**
    ///
    /// Every pattern, not only the active one: song mode can be playing any of
    /// them, and a scheduler that had to ask the non-RT side for a pattern it
    /// did not have would be a scheduler that allocates.
    pub fn compile(&self) -> CompiledSong {
        let state = self.state.lock();
        CompiledSong::new(
            state.patterns.compile(),
            state.patterns.active_index(),
            state.playlist.compile_pattern_clips(&state.patterns),
        )
        // Audio clips travel separately (see `compile_audio`), but they are
        // still part of the arrangement, so SONG mode must run to the end of
        // one rather than stopping at the last pattern clip.
        .extend_to(state.playlist.end_ticks())
    }

    /// The arrangement's **audio** clips, compiled for the audio thread.
    ///
    /// A second compile rather than a field on [`CompiledSong`]: notes and audio
    /// cross the RT wall to two different places -- the scheduler that fills the
    /// block's event buffer, and the Audio Clip node at the head of that file's
    /// mixer strip -- and one value carrying both would have to be published to
    /// both.
    ///
    /// **Non-RT.** Takes the song lock and then reads `sources`; nothing in the
    /// registry ever takes the song lock, so the order is one-way.
    pub fn compile_audio(&self, sources: &AudioSourceRegistry) -> CompiledAudioSong {
        self.compile_audio_with(sources, |_| AudioClipTune::default())
    }

    /// [`compile_audio`](Self::compile_audio) with per-file pitch and region.
    pub fn compile_audio_with(
        &self,
        sources: &AudioSourceRegistry,
        tune_of: impl Fn(u64) -> AudioClipTune,
    ) -> CompiledAudioSong {
        let state = self.state.lock();
        compile_audio_clips_with(&state.playlist, |id| sources.get(id), tune_of)
    }

    /// One past the last tick the arrangement occupies. `0` when it is empty.
    pub fn song_end_ticks(&self) -> u64 {
        self.state.lock().playlist.end_ticks()
    }
}

/// Anything that can go wrong editing the song.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SongError {
    #[error(transparent)]
    Pattern(#[from] PatternError),
    #[error(transparent)]
    PatternSet(#[from] PatternSetError),
    #[error(transparent)]
    Playlist(#[from] PlaylistError),
}

impl std::fmt::Debug for SequencerService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        f.debug_struct("SequencerService")
            .field("bpm", &state.bpm)
            .field("mode", &state.mode)
            .field("patterns", &state.patterns.len())
            .field("active", &state.patterns.active_id())
            .field("clips", &state.playlist.clips().len())
            .finish()
    }
}

/// Provides [`SequencerService`] and contributes the pattern panels.
///
/// No RT node factory: the sequencer is not a node. It runs *before* the graph,
/// filling the block's note buffer, which is why its scheduler lives in
/// `audio-io`'s RT state rather than in the chain (see the spec).
#[derive(Debug, Default)]
pub struct SequencerPlugin;

impl SequencerPlugin {
    pub const fn new() -> Self {
        Self
    }
}

impl Plugin for SequencerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, PLUGIN_NAME)
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        // `provide` only fails on a duplicate, which would mean the plugin was
        // registered twice -- a wiring bug in the shell, not a runtime state.
        ctx.services
            .provide(Arc::new(SequencerService::new()))
            .expect("SequencerService provided twice: the plugin is registered more than once");
        // The audio files clips point at. A separate service rather than a field
        // on `SequencerService`: a decode holds its lock for as long as a disk
        // read takes, and the song's lock is taken by every keystroke in the
        // piano roll. See `crate::audio`.
        ctx.services
            .provide(Arc::new(AudioSourceRegistry::new()))
            .expect("AudioSourceRegistry provided twice: the plugin is registered more than once");
        ctx.contribute_panel(PanelManifest::new(
            SEQUENCER_PANEL,
            "Channel Rack",
            SEQUENCER_PANEL,
        ));
        ctx.contribute_panel(PanelManifest::new(
            PIANO_ROLL_PANEL,
            "Piano Roll",
            PIANO_ROLL_PANEL,
        ));
        ctx.contribute_panel(PanelManifest::new(
            PLAYLIST_PANEL,
            "Playlist",
            PLAYLIST_PANEL,
        ));
    }

    fn start(&mut self, _ctx: &KernelCtx) {}

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;
    use mydaw_rt_core::{MAX_BPM, MIN_BPM};

    const BAR: u64 = TICKS_PER_BAR as u64;

    fn booted() -> Kernel {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(SequencerPlugin::new())).unwrap();
        kernel.boot().unwrap();
        kernel
    }

    fn service() -> SequencerService {
        SequencerService::with_song(
            PatternSet::starter(&[0, 1, 2, 3, 4], |c| {
                DEFAULT_ROW_KEYS
                    .get(c as usize)
                    .copied()
                    .unwrap_or(DEFAULT_STEP_KEY)
            }),
            Playlist::default(),
        )
    }

    #[test]
    fn the_plugin_provides_the_service_and_contributes_all_three_pattern_panels() {
        let kernel = booted();
        assert!(kernel.ctx().services.get::<SequencerService>().is_some());

        let panels = kernel.ui_panels();
        assert_eq!(panels.len(), 3, "the rack, the roll and the playlist");
        assert_eq!(panels[0].id, SEQUENCER_PANEL);
        assert_eq!(panels[0].title, "Channel Rack");
        assert_eq!(panels[1].id, PIANO_ROLL_PANEL);
        assert_eq!(panels[2].id, PLAYLIST_PANEL);
        assert_eq!(panels[2].title, "Playlist");
        // Keys into ui/src/panels/registry.ts -- renaming these breaks the UI.
        assert_eq!(panels[0].component, "step-sequencer");
        assert_eq!(panels[1].component, "piano-roll");
        assert_eq!(panels[2].component, "playlist");
    }

    #[test]
    fn the_plugin_contributes_no_rt_node_factory() {
        // The sequencer fills the block's note buffer before the graph runs; it
        // is not a node, and a factory here would be a lie a project file could
        // later try to instantiate.
        let kernel = booted();
        assert!(kernel.ctx().rt_node_factories.is_empty());
    }

    #[test]
    fn a_fresh_service_is_one_empty_pattern_at_the_default_tempo_in_pattern_mode() {
        let service = service();
        assert_eq!(service.bpm(), DEFAULT_BPM);
        assert_eq!(service.play_mode(), PlayMode::Pattern);
        assert_eq!(service.patterns().len(), 1);
        assert_eq!(service.active_pattern().channels.len(), 5);
        assert!(service.active_pattern().is_empty());
        assert_eq!(service.playlist().tracks().len(), 4);
        assert!(service.playlist().clips().is_empty());
        assert_eq!(service.song_end_ticks(), 0);
        assert!(service.compile().active_pattern().unwrap().is_silent());
    }

    #[test]
    fn a_rejected_pattern_edit_rolls_the_whole_set_back() {
        let service = service();
        let before = service.patterns();

        // An edit that succeeds but leaves the set invalid.
        let err = service
            .edit_patterns(|set| {
                set.get_mut(1)?.length_steps = 24;
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(
            err,
            SongError::Pattern(PatternError::WrongStepCount(24))
        ));
        assert_eq!(service.patterns(), before, "nothing may have changed");

        // An edit that fails outright.
        let err = service.edit_patterns(|set| set.remove(1)).unwrap_err();
        assert!(matches!(
            err,
            SongError::PatternSet(PatternSetError::LastPattern)
        ));
        assert_eq!(service.patterns(), before);
    }

    #[test]
    fn a_rejected_playlist_edit_rolls_the_arrangement_back() {
        let service = service();
        service
            .edit_playlist(|list| list.add_pattern_clip(1, 0, 0, BAR))
            .unwrap();
        let before = service.playlist();

        let err = service
            .edit_playlist(|list| list.add_pattern_clip(1, 9, 0, BAR))
            .unwrap_err();
        assert!(matches!(
            err,
            SongError::Playlist(PlaylistError::NoSuchTrack(9))
        ));
        assert_eq!(service.playlist(), before);

        // A clip naming a pattern that is not in the set fails validation, and
        // the clip that was added is taken back out again.
        let err = service
            .edit_playlist(|list| list.add_pattern_clip(99, 0, 0, BAR))
            .unwrap_err();
        assert!(matches!(
            err,
            SongError::Playlist(PlaylistError::NoSuchPattern { .. })
        ));
        assert_eq!(service.playlist(), before);
    }

    #[test]
    fn deleting_a_pattern_takes_its_clips_with_it_in_one_step() {
        // The set and the playlist must never be separately observable in a
        // state where one names a pattern the other has deleted.
        let service = service();
        let second = service
            .edit_patterns(|set| set.add(Some("B".to_owned()), &[0], |_| 36))
            .unwrap();
        service
            .edit_playlist(|list| list.add_pattern_clip(second, 0, 0, BAR))
            .unwrap();
        service
            .edit_playlist(|list| list.add_pattern_clip(second, 1, BAR, BAR))
            .unwrap();
        service
            .edit_playlist(|list| list.add_pattern_clip(1, 2, 0, BAR))
            .unwrap();

        assert_eq!(service.remove_pattern(second).unwrap(), 2);
        assert_eq!(service.patterns().len(), 1);
        assert_eq!(service.playlist().clips().len(), 1);
        service.playlist().validate(&service.patterns()).unwrap();

        assert!(matches!(
            service.remove_pattern(1),
            Err(SongError::PatternSet(PatternSetError::LastPattern))
        ));
    }

    #[test]
    fn a_rack_channel_appears_in_and_disappears_from_every_pattern() {
        let service =
            SequencerService::with_song(PatternSet::starter(&[0], |_| 36), Playlist::default());
        service
            .edit_patterns(|set| set.add(None, &[0], |_| 36))
            .unwrap();

        service.add_channel(1, 38);
        for pattern in service.patterns().patterns() {
            assert!(pattern.channel(1).is_some(), "{}", pattern.name);
        }

        service.remove_channel(0);
        for pattern in service.patterns().patterns() {
            assert_eq!(
                pattern
                    .channels
                    .iter()
                    .map(|c| c.channel)
                    .collect::<Vec<_>>(),
                vec![0],
                "channel 1 moved down into 0's place"
            );
        }
    }

    #[test]
    fn the_service_clamps_the_tempo_the_same_way_the_clock_does() {
        let service = service();
        assert_eq!(service.set_bpm(174.3), 174.3);
        assert_eq!(service.bpm(), 174.3);
        assert_eq!(service.set_bpm(0.0), MIN_BPM);
        assert_eq!(service.set_bpm(5_000.0), MAX_BPM);
        assert_eq!(service.set_bpm(f64::NAN), DEFAULT_BPM);
    }

    #[test]
    fn the_play_mode_is_session_state_and_survives_with_no_device() {
        let service = service();
        assert_eq!(service.play_mode(), PlayMode::Pattern);
        service.set_play_mode(PlayMode::Song);
        assert_eq!(service.play_mode(), PlayMode::Song);
    }

    #[test]
    fn compiling_carries_every_pattern_the_active_index_and_the_arrangement() {
        let service = service();
        let second = service
            .edit_patterns(|set| set.add(Some("B".to_owned()), &[0], |_| 38))
            .unwrap();
        service
            .edit_patterns(|set| {
                set.get_mut(1)?.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 8]));
                Ok(())
            })
            .unwrap();
        service.edit_patterns(|set| set.set_active(second)).unwrap();
        service
            .edit_playlist(|list| list.add_pattern_clip(1, 0, 4 * BAR, 4 * BAR))
            .unwrap();

        let song = service.compile();
        assert_eq!(song.pattern_count(), 2);
        assert_eq!(song.active(), 1, "the second pattern is being edited");
        assert_eq!(song.active_pattern().unwrap().id(), second);
        assert_eq!(song.clips().len(), 1);
        assert_eq!(song.clips()[0].pattern, 0, "resolved from the id");
        assert_eq!(song.clips()[0].start_ticks, 4 * BAR);
        assert_eq!(song.end_ticks(), 8 * BAR);
        assert_eq!(service.song_end_ticks(), 8 * BAR);
    }

    #[test]
    fn replacing_the_whole_song_refuses_one_the_engine_could_not_play() {
        let service = service();
        let before = service.song();

        let mut playlist = Playlist::default();
        playlist.add_pattern_clip(99, 0, 0, BAR).unwrap();
        assert!(matches!(
            service.set_song(PatternSet::default(), playlist),
            Err(SongError::Playlist(PlaylistError::NoSuchPattern { .. }))
        ));
        assert_eq!(service.song(), before, "a refused load changes nothing");

        // ...and a valid one goes through.
        let mut good = Playlist::default();
        good.add_pattern_clip(1, 0, 0, BAR).unwrap();
        service.set_song(PatternSet::default(), good).unwrap();
        assert_eq!(service.playlist().clips().len(), 1);
    }

    #[test]
    fn the_manifest_names_the_plugin_stably() {
        assert_eq!(SequencerPlugin::new().manifest().id, PLUGIN_ID);
        assert_eq!(PLUGIN_ID, "sequencer");
        assert_eq!(SEQUENCER_PANEL, "step-sequencer");
        assert_eq!(PLAYLIST_PANEL, "playlist");
    }

    #[test]
    fn the_debug_line_says_what_the_session_holds() {
        let service = service();
        let text = format!("{service:?}");
        assert!(text.contains("patterns: 1"), "{text}");
        assert!(text.contains("Pattern"), "{text}");
        assert!(text.len() < 250);
    }

    #[test]
    fn split_by_channel_spreads_playlist_clips_and_adds_lanes() {
        let service = SequencerService::with_song(
            PatternSet::starter(&[0, 1], |c| if c == 0 { 36 } else { 38 }),
            Playlist::empty(),
        );
        service
            .edit_patterns(|set| {
                set.get_mut(1)?.name = "Loop".to_owned();
                set.get_mut(1)?.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0]));
                set.get_mut(1)?.channels[1].steps = Some(StepRow::empty(38).with_steps(&[4]));
                Ok(())
            })
            .unwrap();
        service
            .edit_playlist(|list| list.add_track(Some("A".into())))
            .unwrap();
        service
            .edit_playlist(|list| list.add_pattern_clip(1, 0, 0, BAR))
            .unwrap();

        let ids = service
            .split_pattern_by_channel(1, |ch| format!("ch{ch}"))
            .unwrap();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(service.patterns().len(), 2);
        assert_eq!(service.patterns().get(1).unwrap().name, "Loop - ch0");
        assert_eq!(service.patterns().get(2).unwrap().name, "Loop - ch1");
        assert_eq!(service.playlist().tracks().len(), 2);
        assert_eq!(service.playlist().clips().len(), 2);
        assert_eq!(service.playlist().clips()[1].track(), 1);
    }
}
