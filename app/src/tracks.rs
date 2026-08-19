//! The track host: the shell's runtime view of the mixer.
//!
//! [`Project`] is the data; this is the machine that keeps the audio thread
//! agreeing with it. It owns three things that have to stay in step:
//!
//! ```text
//!   Project (Mutex)  ---- publish ----> MixerParams (atomics) --> strips
//!        |                                                          ^
//!        |  per track                                               |
//!        +----> SourceHandle -- rtrb ------> SourceSlot ------------+
//!                                            (head of the chain)
//! ```
//!
//! # Two kinds of edit, and why only one rebuilds the graph
//!
//! **Mixer edits** -- gain, pan, mute, solo, master -- are stores into
//! [`MixerParams`]. Nothing is rebuilt, nothing is allocated, and the audio
//! thread hears them on its next block with a 10 ms ramp. That is what makes a
//! fader drag cost the same as a fader click.
//!
//! **Source changes** -- "make track 1 a drum kit" -- are a node swap through
//! that track's [`SourceHandle`], exactly as in step 3, and also do not rebuild
//! the graph. The reason is the same one that made step 3 do it this way: a
//! hosted CLAP plugin can only be activated once at a time, so a rebuild that
//! recreates every node cannot keep one alive across itself.
//!
//! **Layout changes** -- adding or removing a track -- are the only edit that
//! compiles and installs a new graph, because the number of track chains is
//! fixed once a graph is compiled. Step 5 refused them while a hosted CLAP
//! plugin was in the graph; step 6 allows them, by having the main thread
//! install a CLAP-free graph and then re-attach every instance into it. That
//! protocol lives in [`ClapHost::rebuild`](crate::clap_host::ClapHost), because
//! only the main thread can touch a CLAP instance -- this type only has to make
//! sure a rebuilt graph has a *slot* waiting for each one.
//!
//! # Where a hosted effect goes
//! The master bus carries one always-present [`InsertSlot`](crate::insert::InsertSlot),
//! empty until a plugin is chosen. It runs on the summed bus, after every track
//! strip and before the master fader -- where a master-bus effect belongs, and
//! the only insert point that does not have to pick a track for it. Per-track
//! *effect* inserts are a later step's work; the graph already carries a
//! per-track insert chain, so that is a change to the builder rather than to the
//! engine.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use mydaw_plugin_audio_io::{AudioIoError, AudioIoService};
use mydaw_plugin_instruments::InstrumentRegistry;
use mydaw_plugin_sampler::SamplerRegistry;
use mydaw_plugin_test_tone::TestToneFactory;
use mydaw_rt_core::basedrop::Handle;
use mydaw_rt_core::{GraphSpec, MixerParams, RtNode, RtNodeFactory, MAX_TRACKS};
use parking_lot::Mutex;

use crate::clap_bridge::ClapBoard;
use crate::graph::track_chain;
use crate::insert::InsertHandle;
use crate::project::{
    check_gain_db, check_pan, mixer_state_dto, MissingPlugins, MixerStateDto, Project, TrackDef,
    TrackError,
};
use crate::source::{SilentNode, Source, SourceError, SourceHandle};
use crate::vst3_bridge::Vst3Board;

/// Anything that can go wrong editing the mixer.
#[derive(Debug, thiserror::Error)]
pub enum MixerError {
    /// The edit itself was refused: a bad index, a bad value, a layout change
    /// that cannot be made.
    #[error(transparent)]
    Track(#[from] TrackError),
    /// The source could not be built or installed.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// No stream, or the audio thread is behind.
    #[error(transparent)]
    Audio(#[from] AudioIoError),
}

/// The shell's mixer. `Send + Sync`, so every Tauri command reaches it directly.
pub struct TrackHost {
    tone: Arc<TestToneFactory>,
    instruments: Arc<InstrumentRegistry>,
    /// The sampled instruments, which unlike the synthesised ones are
    /// discovered at runtime -- and may appear *after* this host was built,
    /// when a download finishes. Held as the registry rather than as a list of
    /// factories for exactly that reason.
    sampler: Arc<SamplerRegistry>,
    /// What the audio thread reads. Shared with every graph compiled from this
    /// host, so a rebuild never resets a fader.
    params: Arc<MixerParams>,
    project: Mutex<Project>,
    /// One head-of-chain handle per track *slot*, created once and reused for
    /// the process lifetime -- a handle outlives the streams it is attached to
    /// (see [`SourceHandle`]), so there is nothing to rebuild here either.
    handles: Vec<Arc<SourceHandle>>,
    /// Per-track transport-gate flags, published by the slot and read by the
    /// gate node in the same chain.
    gates: Vec<Arc<AtomicBool>>,
    /// The one insert point on the master bus. Present in every graph, empty
    /// until a plugin is chosen -- see [`crate::insert`].
    master_insert: Arc<InsertHandle>,
    /// Track the keyboard piano and the source selector address.
    selected: AtomicUsize,
}

impl TrackHost {
    /// A host on the default project.
    pub fn new(
        tone: Arc<TestToneFactory>,
        instruments: Arc<InstrumentRegistry>,
        sampler: Arc<SamplerRegistry>,
    ) -> Self {
        Self::with_project(tone, instruments, sampler, Project::default())
    }

    /// A host on an explicit project (tests, and one day a project file).
    pub fn with_project(
        tone: Arc<TestToneFactory>,
        instruments: Arc<InstrumentRegistry>,
        sampler: Arc<SamplerRegistry>,
        project: Project,
    ) -> Self {
        let params = Arc::new(MixerParams::new());
        project.publish(&params);
        Self {
            tone,
            instruments,
            sampler,
            params,
            handles: (0..MAX_TRACKS)
                .map(|index| {
                    let source = project
                        .tracks
                        .get(index)
                        .map_or(Source::DEFAULT, |track| track.source.clone());
                    Arc::new(SourceHandle::new(source))
                })
                .collect(),
            gates: (0..MAX_TRACKS)
                .map(|_| Arc::new(AtomicBool::new(false)))
                .collect(),
            master_insert: Arc::new(InsertHandle::new()),
            selected: AtomicUsize::new(0),
            project: Mutex::new(project),
        }
    }

    /// The parameter block the audio thread reads.
    pub fn params(&self) -> &Arc<MixerParams> {
        &self.params
    }

    /// The master bus's insert point.
    ///
    /// The main thread installs a hosted CLAP effect here and clears it again;
    /// nothing else writes to it. `Send + Sync`, like everything else on this
    /// type, because the *handle* is -- only the node it carries is not.
    pub fn master_insert(&self) -> &Arc<InsertHandle> {
        &self.master_insert
    }

    /// A copy of the project.
    pub fn project(&self) -> Project {
        self.project.lock().clone()
    }

    /// Replace the project wholesale and republish its parameters.
    ///
    /// Used by the offline entry points, which want a project of exactly one
    /// known track, and by the startup path, which hands its own project back.
    /// The graph is **not** rebuilt here -- the caller installs one, because
    /// installing needs an open stream and this does not.
    pub fn set_project(&self, project: Project) {
        let mut guard = self.project.lock();
        *guard = project;
        if self.selected.load(Ordering::Relaxed) >= guard.len() {
            self.selected
                .store(guard.len().saturating_sub(1), Ordering::Relaxed);
        }
        guard.publish(&self.params);
    }

    /// Tracks in the project.
    pub fn track_count(&self) -> usize {
        self.project.lock().len()
    }

    /// The track the keyboard piano plays, clamped to what exists.
    pub fn selected(&self) -> usize {
        let selected = self.selected.load(Ordering::Relaxed);
        let count = self.project.lock().len();
        if count == 0 {
            0
        } else {
            selected.min(count - 1)
        }
    }

    /// The source at the head of the selected track.
    pub fn selected_source(&self) -> Source {
        let project = self.project.lock();
        project
            .tracks
            .get(
                self.selected
                    .load(Ordering::Relaxed)
                    .min(project.len().saturating_sub(1)),
            )
            .map_or(Source::DEFAULT, |track| track.source.clone())
    }

    /// The head-of-chain handle for `track`.
    pub fn handle(&self, track: usize) -> Option<&Arc<SourceHandle>> {
        self.handles.get(track)
    }

    /// Whether a hosted CLAP node is currently in the graph, as a track source
    /// or as the master insert.
    ///
    /// Not a restriction any more (step 5 refused layout edits while it was
    /// true); it is what decides whether a layout edit can be done inline on the
    /// calling thread or has to go through the main thread's re-attach protocol.
    pub fn has_clap_node(&self) -> bool {
        self.master_insert.is_occupied() || self.project.lock().has_clap_source()
    }

    /// Whether a hosted plugin of **either** format is currently in the graph.
    ///
    /// What decides whether a layout edit can be done inline on the calling
    /// thread or has to go through a main-thread re-attach.
    pub fn has_hosted_node(&self) -> bool {
        self.master_insert.is_occupied() || self.project.lock().has_hosted_source()
    }

    /// Whether a hosted VST3 node heads a track. The master insert is not
    /// counted, because its format is not knowable from here -- the boards are
    /// what answer that.
    pub fn has_vst3_source(&self) -> bool {
        self.project.lock().has_vst3_source()
    }

    /// The mixer, in the shape the webview reads.
    pub fn state(
        &self,
        clap: &ClapBoard,
        vst3: &Vst3Board,
        missing: &MissingPlugins,
    ) -> MixerStateDto {
        let project = self.project.lock();
        let selected = self
            .selected
            .load(Ordering::Relaxed)
            .min(project.len().saturating_sub(1));
        mixer_state_dto(&project, selected, clap, vst3, missing)
    }

    // ------------------------------------------------------------ mixer edits

    /// Set a track's fader, in dB.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`] or [`TrackError::InvalidGain`]. Never an
    /// audio error: a fader is project data, and moving one with no device open
    /// has to work.
    pub fn set_gain_db(&self, track: usize, db: f32) -> Result<(), TrackError> {
        let db = check_gain_db(db)?;
        self.edit(track, |def| def.gain_db = db)
    }

    /// Set a track's pan, `-1.0..=1.0`.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`] or [`TrackError::InvalidPan`].
    pub fn set_pan(&self, track: usize, pan: f32) -> Result<(), TrackError> {
        let pan = check_pan(pan)?;
        self.edit(track, |def| def.pan = pan)
    }

    /// Mute or unmute a track.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_muted(&self, track: usize, muted: bool) -> Result<(), TrackError> {
        self.edit(track, |def| def.muted = muted)
    }

    /// Solo or un-solo a track. Republishes every track's effective mute.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_soloed(&self, track: usize, soloed: bool) -> Result<(), TrackError> {
        self.edit(track, |def| def.soloed = soloed)
    }

    /// Set the master fader, in dB.
    ///
    /// # Errors
    /// [`TrackError::InvalidGain`].
    pub fn set_master_gain_db(&self, db: f32) -> Result<(), TrackError> {
        let db = check_gain_db(db)?;
        let mut project = self.project.lock();
        project.master_gain_db = db;
        project.publish(&self.params);
        Ok(())
    }

    /// Choose the track the keyboard piano and the source selector address.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_selected(&self, track: usize) -> Result<(), TrackError> {
        if track >= self.project.lock().len() {
            return Err(TrackError::NoSuchTrack(track));
        }
        self.selected.store(track, Ordering::Relaxed);
        Ok(())
    }

    /// Apply `edit` to one track and republish the whole project.
    ///
    /// Republishing everything rather than the field that changed is deliberate:
    /// solo makes every track's effective mute depend on every other track's
    /// state, so "which atomics did this edit touch" is not a local question.
    /// Three relaxed stores per track is nothing next to getting it wrong.
    fn edit(&self, track: usize, edit: impl FnOnce(&mut TrackDef)) -> Result<(), TrackError> {
        let mut project = self.project.lock();
        let def = project
            .tracks
            .get_mut(track)
            .ok_or(TrackError::NoSuchTrack(track))?;
        edit(def);
        project.publish(&self.params);
        Ok(())
    }

    // ----------------------------------------------------------- source edits

    /// Build the RT node for `source`.
    ///
    /// A hosted source -- [`Source::Clap`] or [`Source::Vst3`] -- produces
    /// [`SilentNode`] rather than an error: only its host's main thread can
    /// activate a plugin, so a compiled graph carries a silent placeholder in the
    /// track's slot and the main thread swaps the real node in afterwards (at
    /// startup, and after every rebuild). Silence rather than a fallback
    /// instrument, because a track set to a synthesiser must not answer with a
    /// sine wave -- see [`SilentNode`].
    ///
    /// A [`Source::Sampler`] whose instrument is not loaded also produces
    /// [`SilentNode`], with a warning. That is a *runtime* condition rather
    /// than a wiring bug -- a project saved on a machine with the sample
    /// content opened on one without it -- and silence is the same answer given
    /// for a plugin that will not load, for the same reason: a track set to a
    /// piano must not answer with a sine wave.
    ///
    /// # Errors
    /// [`SourceError::MissingInstrument`] when the instruments plugin did not
    /// contribute the node a built-in source names -- a wiring bug rather than a
    /// runtime condition.
    pub fn build_node(
        &self,
        source: &Source,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Box<dyn RtNode>, SourceError> {
        match source {
            Source::TestTone => Ok(self.tone.create(sample_rate, max_block)),
            Source::Clap(_) | Source::Vst3(_) => Ok(Box::new(SilentNode)),
            Source::Sampler(reference) => {
                let type_id = reference.wire_id();
                Ok(self
                    .sampler
                    .create(&type_id, sample_rate, max_block)
                    .unwrap_or_else(|| {
                        log::warn!(
                            "the sampled instrument '{}' is not installed; \
                             this track will be silent until its assets are downloaded",
                            reference.instrument_id
                        );
                        Box::new(SilentNode)
                    }))
            }
            other => {
                let type_id = other
                    .instrument_type_id()
                    .expect("every non-hosted, non-tone source names a built-in instrument");
                self.instruments
                    .create(type_id, sample_rate, max_block)
                    .ok_or(SourceError::MissingInstrument(type_id))
            }
        }
    }

    /// Put a different source at the head of `track`, while the engine runs.
    ///
    /// A node swap, not a rebuild -- see the module docs.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`] and [`AudioIoError::NotRunning`] with no
    /// device open. A hosted CLAP source does not come through here at all: it
    /// goes through [`ClapHostBridge`](crate::clap_bridge::ClapHostBridge), so
    /// that the main thread creates the instance.
    pub fn set_source(
        &self,
        track: usize,
        source: Source,
        audio: &AudioIoService,
    ) -> Result<(), MixerError> {
        if track >= self.project.lock().len() {
            return Err(TrackError::NoSuchTrack(track).into());
        }
        let info = audio.stream_info().ok_or(AudioIoError::NotRunning)?;
        let node = self.build_node(&source, f64::from(info.sample_rate), info.max_block)?;
        self.install_source(track, source, node)?;
        Ok(())
    }

    /// Install an already-built node as `track`'s source and record it.
    ///
    /// The path the main thread takes for the hosted CLAP instrument, which only
    /// it can build. Recording the source in the project *after* the install
    /// succeeds is what keeps a failed swap from changing what the UI is told is
    /// playing.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], or whatever the swap queue reported.
    pub fn install_source(
        &self,
        track: usize,
        source: Source,
        node: Box<dyn RtNode>,
    ) -> Result<(), MixerError> {
        let handle = self
            .handles
            .get(track)
            .ok_or(TrackError::NoSuchTrack(track))?;
        handle.install(node, source.clone())?;

        let mut project = self.project.lock();
        let Some(def) = project.tracks.get_mut(track) else {
            return Err(TrackError::NoSuchTrack(track).into());
        };
        def.source = source;
        Ok(())
    }

    // ----------------------------------------------------------- layout edits

    /// Append a track and rebuild the graph.
    ///
    /// **A rebuild leaves every hosted CLAP plugin without a node.** The new
    /// graph's slots are empty placeholders (see
    /// [`build_node`](Self::build_node)); re-attaching the instances is the main
    /// thread's job and is why a layout edit with a plugin loaded is routed
    /// through [`ClapHost`](crate::clap_host::ClapHost) rather than called from
    /// a command thread. [`AppState::add_track`](crate::AppState::add_track) is
    /// what makes that choice.
    ///
    /// # Errors
    /// [`TrackError::TooManyTracks`], and whatever building or installing the
    /// new graph reported.
    pub fn add_track(
        &self,
        source: Source,
        name: Option<String>,
        audio: &AudioIoService,
    ) -> Result<usize, MixerError> {
        let index = {
            let mut project = self.project.lock();
            let index = project.add(source, name)?;
            project.publish(&self.params);
            index
        };
        self.rebuild(audio)?;
        Ok(index)
    }

    /// Remove a track and rebuild the graph.
    ///
    /// See [`add_track`](Self::add_track) on what a rebuild does to hosted
    /// plugins.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], [`TrackError::LastTrack`], and whatever the
    /// rebuild reported.
    pub fn remove_track(&self, track: usize, audio: &AudioIoService) -> Result<(), MixerError> {
        {
            let mut project = self.project.lock();
            project.remove(track)?;
            project.publish(&self.params);
        }
        // The selection may now point past the end.
        let count = self.project.lock().len();
        if self.selected.load(Ordering::Relaxed) >= count {
            self.selected
                .store(count.saturating_sub(1), Ordering::Relaxed);
        }
        self.rebuild(audio)
    }

    // ---------------------------------------------------------- graph install

    /// Compile the project into a graph and hand it to the audio thread.
    ///
    /// The graph contains **no hosted CLAP node**: a CLAP-headed track gets a
    /// silent placeholder and the master insert slot starts empty. Only the main
    /// thread can activate a plugin, and it does so afterwards, through the slots
    /// this graph carries.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] with no stream open, plus whatever the graph
    /// queue reported. A track whose built-in source cannot be built falls back
    /// to the test tone with a warning rather than leaving the stream silent.
    pub fn install_graph(&self, audio: &AudioIoService) -> Result<(), AudioIoError> {
        let limits = audio.graph_limits().ok_or(AudioIoError::NotRunning)?;
        let gc = audio.gc_handle();
        let spec = self.build_spec(&limits, gc);
        audio.install_graph(spec)
    }

    /// Rebuild. See [`install_graph`](Self::install_graph).
    fn rebuild(&self, audio: &AudioIoService) -> Result<(), MixerError> {
        self.install_graph(audio)?;
        Ok(())
    }

    /// Assemble the graph for the current project.
    ///
    /// Split out of [`install_graph`](Self::install_graph) so an offline test can
    /// render the graph the shell actually runs rather than a copy of it that
    /// can drift.
    pub fn build_spec(&self, limits: &mydaw_rt_core::GraphLimits, gc: Handle) -> GraphSpec {
        let project = self.project.lock();
        project.publish(&self.params);

        let mut spec = GraphSpec::new()
            .with_params(Arc::clone(&self.params))
            .with_limits(*limits);

        for (index, def) in project.tracks.iter().enumerate() {
            let node = match self.build_node(&def.source, limits.sample_rate, limits.max_block) {
                Ok(node) => node,
                Err(err) => {
                    log::warn!(
                        "track {index} ('{}'): {err}; falling back to the test tone",
                        def.name
                    );
                    self.tone.create(limits.sample_rate, limits.max_block)
                }
            };
            let gate = Arc::clone(&self.gates[index]);
            let slot =
                self.handles[index].open(node, def.source.clone(), gc.clone(), Arc::clone(&gate));
            // Per-track *effect* inserts are a later step's work -- the graph
            // already carries a chain for them, and this is where they would go.
            spec.push_track(track_chain(slot, None, gate));
        }

        // Unconditional, and transparent while empty. Compiling it in every time
        // is what lets a master effect be inserted, replaced and removed with no
        // graph rebuild at all -- see `crate::insert`.
        spec.push_master_insert(crate::insert::insert_slot(&self.master_insert, gc));
        spec
    }

    /// Forget every track's stream channel, and the master insert's. Called when
    /// audio stops.
    pub fn close(&self) {
        for handle in &self.handles {
            handle.close();
        }
        self.master_insert.close();
    }
}

impl std::fmt::Debug for TrackHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackHost")
            .field("tracks", &self.track_count())
            .field("selected", &self.selected())
            .field("clap_in_graph", &self.has_clap_node())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ClapSourceRef;
    use mydaw_rt_core::{db_to_gain, GraphLimits, MAX_TRACKS};

    const LIMITS: GraphLimits = GraphLimits {
        sample_rate: 48_000.0,
        max_block: 256,
        max_events: 64,
    };

    fn clap_ref() -> ClapSourceRef {
        ClapSourceRef::new(r"C:\plugins\thing.clap", "com.acme.thing").named("Acme Thing")
    }

    fn host() -> TrackHost {
        TrackHost::new(
            Arc::new(mydaw_plugin_test_tone::TestToneFactory::new()),
            Arc::new(InstrumentRegistry::new()),
            Arc::new(SamplerRegistry::detached()),
        )
    }

    #[test]
    fn a_fresh_host_publishes_the_default_project_before_anything_asks() {
        // The parameter block has to be right from the first block the audio
        // thread runs, which is before any command has arrived.
        let host = host();
        assert_eq!(host.track_count(), 2);
        assert_eq!(host.selected(), 0);
        assert_eq!(host.selected_source(), Source::DrumKit);

        let params = host.params();
        assert_eq!(params.track_gain(0), 1.0);
        assert!(!params.track_muted(0));
        assert!(!params.track_muted(1));
        assert!(
            params.track_muted(2),
            "a slot with no track behind it must be silent"
        );
        assert_eq!(params.master_gain(), 1.0);
    }

    #[test]
    fn a_mixer_edit_reaches_the_atomics_with_no_device_and_no_graph() {
        let host = host();
        host.set_gain_db(1, -12.0).unwrap();
        host.set_pan(1, -0.5).unwrap();
        host.set_master_gain_db(-3.0).unwrap();

        assert!((host.params().track_gain(1) - db_to_gain(-12.0)).abs() < 1e-6);
        assert_eq!(host.params().track_pan(1), -0.5);
        assert!((host.params().master_gain() - db_to_gain(-3.0)).abs() < 1e-6);

        // ...and the project remembers it, because that is what gets saved.
        assert_eq!(host.project().tracks[1].gain_db, -12.0);
        assert_eq!(host.project().master_gain_db, -3.0);
    }

    #[test]
    fn solo_is_resolved_into_the_mutes_the_audio_thread_reads() {
        let host = host();
        host.set_muted(0, true).unwrap();
        assert!(host.params().track_muted(0));
        assert!(!host.params().track_muted(1));

        host.set_soloed(1, true).unwrap();
        assert!(host.params().track_muted(0), "not soloed");
        assert!(!host.params().track_muted(1));

        host.set_soloed(1, false).unwrap();
        assert!(host.params().track_muted(0), "its own mute is still on");
        assert!(!host.params().track_muted(1));
    }

    #[test]
    fn an_index_the_webview_should_never_have_sent_is_named_rather_than_ignored() {
        let host = host();
        for outcome in [
            host.set_gain_db(9, 0.0),
            host.set_pan(9, 0.0),
            host.set_muted(9, true),
            host.set_soloed(9, true),
            host.set_selected(9),
        ] {
            assert!(matches!(outcome, Err(TrackError::NoSuchTrack(9))));
        }
        assert_eq!(host.selected(), 0, "a refused selection changes nothing");
        assert!(matches!(
            host.set_gain_db(0, f32::NAN),
            Err(TrackError::InvalidGain(_))
        ));
    }

    #[test]
    fn the_selected_track_is_what_the_keyboard_and_the_selector_address() {
        let host = host();
        assert_eq!(host.selected_source(), Source::DrumKit);
        host.set_selected(1).unwrap();
        assert_eq!(host.selected(), 1);
        assert_eq!(host.selected_source(), Source::PolySynth);
        let board = ClapBoard::new();
        let missing = MissingPlugins::new();
        assert!(host.state(&board, &Vst3Board::new(), &missing).tracks[1].selected);
        assert!(!host.state(&board, &Vst3Board::new(), &missing).tracks[0].selected);
    }

    #[test]
    fn replacing_the_project_clamps_a_selection_that_no_longer_exists() {
        let host = host();
        host.set_selected(1).unwrap();
        let mut single = Project::empty();
        single.add(Source::TestTone, None).unwrap();
        host.set_project(single);
        assert_eq!(host.selected(), 0);
        assert!(
            host.params().track_muted(1),
            "the slot the removed track used must be silent"
        );
    }

    #[test]
    fn the_graph_the_host_builds_is_one_track_chain_per_track_plus_the_master_insert() {
        let host = host();
        let collector = mydaw_rt_core::basedrop::Collector::new();
        let spec = host.build_spec(&LIMITS, collector.handle());

        assert_eq!(spec.track_count(), 2);
        // Source slot plus transport gate, twice, plus the master insert slot.
        assert_eq!(spec.len(), 5);
        assert!(spec.params().is_some(), "the mixer parameters are shared");
        assert_eq!(spec.limits(), LIMITS);
        assert!(!host.has_clap_node());
        assert!(
            host.master_insert().is_open() && !host.master_insert().is_occupied(),
            "the slot is there, and empty"
        );
    }

    #[test]
    fn a_layout_edit_is_allowed_with_a_clap_plugin_placed() {
        // The step 5 limitation, deleted. The project edit goes through; the
        // rebuild then fails only because this test has no audio device.
        let host = host();
        let mut project = host.project();
        project.tracks[0].source = Source::Clap(clap_ref());
        host.set_project(project);
        assert!(host.has_clap_node());

        let audio = mydaw_plugin_audio_io::AudioIoService::default();
        assert!(matches!(
            host.add_track(Source::PolySynth, None, &audio),
            Err(MixerError::Audio(AudioIoError::NotRunning))
        ));
        assert_eq!(host.track_count(), 3, "the project edit happened");
    }

    #[test]
    fn a_clap_headed_track_compiles_to_silence_rather_than_a_fallback_instrument() {
        // The main thread swaps the real node in a moment later. Until it does,
        // a track the user set to a synthesiser must be silent -- not a sine.
        let host = host();
        let mut buffers = mydaw_rt_core::AudioBuffers::new(8);
        for channel in buffers.as_mut_slices() {
            channel.fill(0.5);
        }
        let mut node = host
            .build_node(&Source::Clap(clap_ref()), 48_000.0, 8)
            .expect("a placeholder is always buildable");
        {
            let mut ctx = mydaw_rt_core::ProcessCtx::new(
                &mut buffers,
                48_000.0,
                8,
                mydaw_rt_core::TransportSnapshot::default(),
            );
            node.process(&mut ctx);
        }
        assert!(buffers.as_slices()[0].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn a_layout_edit_without_a_device_fails_after_the_project_was_updated() {
        // Adding a track needs a graph install, which needs a stream. The
        // project is edited first so the UI's refresh shows the truth, and the
        // install failure is reported as itself.
        let host = host();
        let audio = mydaw_plugin_audio_io::AudioIoService::default();
        assert!(matches!(
            host.add_track(Source::PolySynth, None, &audio),
            Err(MixerError::Audio(AudioIoError::NotRunning))
        ));
        assert_eq!(host.track_count(), 3);
        assert!(!host.params().track_muted(2), "the new track is published");
    }

    #[test]
    fn every_built_in_source_can_be_built_from_a_command_thread() {
        let host = host();
        for source in Source::BUILT_IN {
            assert!(
                host.build_node(&source, 48_000.0, 256).is_ok(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn the_track_limit_is_the_engines_and_the_last_track_cannot_be_removed() {
        let host = host();
        let audio = mydaw_plugin_audio_io::AudioIoService::default();
        // No device, so every add reports `NotRunning` -- but the project edit
        // in front of it is what is under test here.
        while host.track_count() < MAX_TRACKS {
            let _ = host.add_track(Source::TestTone, None, &audio);
        }
        assert!(matches!(
            host.add_track(Source::TestTone, None, &audio),
            Err(MixerError::Track(TrackError::TooManyTracks))
        ));

        let mut single = Project::empty();
        single.add(Source::TestTone, None).unwrap();
        host.set_project(single);
        assert!(matches!(
            host.remove_track(0, &audio),
            Err(MixerError::Track(TrackError::LastTrack))
        ));
    }
}
