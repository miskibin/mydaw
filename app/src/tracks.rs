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
use mydaw_plugin_effects::EffectRegistry;
use mydaw_plugin_instruments::InstrumentRegistry;
use mydaw_plugin_sampler::{file_path_of, SamplerRegistry};
use mydaw_plugin_sequencer::SamplePlaybackParams;
use mydaw_plugin_test_tone::TestToneFactory;
use mydaw_rt_core::basedrop::Handle;
use mydaw_rt_core::{BusSpec, GraphSpec, MixerParams, RtNode, RtNodeFactory, SendSpec, MAX_TRACKS};
use parking_lot::Mutex;

/// The longest fade a channel may have, in milliseconds.
///
/// Ten seconds. Long enough for anything anyone would call a fade, short enough
/// that a knob dragged to the top is still a fade rather than a mute -- and a
/// bound is what keeps a NaN or a typo from silencing a channel for a minute.
pub const MAX_FADE_MS: f32 = 10_000.0;

use crate::clap_bridge::ClapBoard;
use crate::effects::{InsertDef, InsertError, InsertTarget};
use crate::graph::track_chain;
use crate::insert::InsertHandle;
use crate::project::{
    check_gain_db, check_pan, mixer_state_dto, MissingPlugins, MixerStateDto, Project, RouteError,
    TrackDef, TrackError,
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
    /// A bus or a send edit was refused -- most usefully, it would have made a
    /// cycle.
    #[error(transparent)]
    Route(#[from] RouteError),
    /// An insert-chain edit was refused.
    #[error(transparent)]
    Insert(#[from] InsertError),
    /// The source could not be built or installed.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// No stream, or the audio thread is behind.
    #[error(transparent)]
    Audio(#[from] AudioIoError),
}

impl MixerError {
    /// Whether the **edit landed** and only publishing it to the audio thread
    /// was refused, because no stream is open.
    ///
    /// A mixer edit changes the project first and installs a new graph second.
    /// With no device the second step fails, but the project has already moved
    /// -- which is why `add_track` returns the index anyway and `publish_song`
    /// treats the same condition as success. A caller that needs the project to
    /// be right (a sample edit, an offline render) checks this instead of
    /// undoing work that did happen.
    pub fn is_only_no_device(&self) -> bool {
        matches!(
            self,
            Self::Audio(AudioIoError::NotRunning)
                | Self::Source(SourceError::Audio(AudioIoError::NotRunning))
        )
    }
}

/// A graph built for an offline render, plus the handles that keep it alive.
///
/// See [`TrackHost::build_offline_spec`]. The handles are inert — nothing ever
/// installs through them — but they own the non-RT end of each track's source
/// queue, and a dropped sender abandons that queue.
pub struct OfflineGraph {
    pub spec: GraphSpec,
    /// Hold these for as long as the graph is being rendered.
    pub handles: Vec<Arc<SourceHandle>>,
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
    /// The internal effects a chain can hold. Consulted to build a chain's
    /// nodes, to answer "what knobs does this have", and to reject a `type_id`
    /// this build does not know.
    effects: Arc<EffectRegistry>,
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
    /// The audio-clip player, which sits on the master bus *before* the insert
    /// point. Present in every graph for the same reason the insert slot is --
    /// see [`crate::clips`]. Owned here because this is what assembles the
    /// graph, not because a clip belongs to a track: a playlist lane is not a
    /// mixer strip.
    audio_clips: Arc<crate::clips::AudioClipBus>,
    /// Browser file preview, on the master bus after the clip player. Present
    /// in every graph so clicking a sample never rebuilds anything.
    file_preview: Arc<crate::file_preview::FilePreviewBus>,
    /// Track the keyboard piano and the source selector address.
    selected: AtomicUsize,
}

impl TrackHost {
    /// A host on the starter project: four drum channels and a melodic one.
    ///
    /// The melodic channel is the sampled piano when its assets are installed
    /// and the poly synth when they are not -- which is why this needs the
    /// registry and [`Project::default`] does not have it. See
    /// [`default_piano`](crate::project::default_piano).
    pub fn new(
        tone: Arc<TestToneFactory>,
        instruments: Arc<InstrumentRegistry>,
        sampler: Arc<SamplerRegistry>,
        effects: Arc<EffectRegistry>,
    ) -> Self {
        let project = Project::starter(crate::project::default_piano(&sampler));
        Self::with_project(tone, instruments, sampler, effects, project)
    }

    /// A host on an explicit project (tests, and one day a project file).
    pub fn with_project(
        tone: Arc<TestToneFactory>,
        instruments: Arc<InstrumentRegistry>,
        sampler: Arc<SamplerRegistry>,
        effects: Arc<EffectRegistry>,
        project: Project,
    ) -> Self {
        let params = Arc::new(MixerParams::new());
        project.publish(&params);
        Self {
            tone,
            instruments,
            sampler,
            effects,
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
            audio_clips: Arc::new(crate::clips::AudioClipBus::new()),
            file_preview: Arc::new(crate::file_preview::FilePreviewBus::new()),
            selected: AtomicUsize::new(0),
            project: Mutex::new(project),
        }
    }

    /// The parameter block the audio thread reads.
    pub fn params(&self) -> &Arc<MixerParams> {
        &self.params
    }

    /// The internal effect catalogue.
    pub fn effects(&self) -> &Arc<EffectRegistry> {
        &self.effects
    }

    /// The master bus's insert point.
    ///
    /// The main thread installs a hosted CLAP effect here and clears it again;
    /// nothing else writes to it. `Send + Sync`, like everything else on this
    /// type, because the *handle* is -- only the node it carries is not.
    pub fn master_insert(&self) -> &Arc<InsertHandle> {
        &self.master_insert
    }

    /// The audio-clip bus: one node per Audio Clip channel, plus one-shot players.
    pub fn audio_clips(&self) -> &Arc<crate::clips::AudioClipBus> {
        &self.audio_clips
    }

    /// The browser preview player. See [`crate::file_preview`].
    pub fn file_preview(&self) -> &Arc<crate::file_preview::FilePreviewBus> {
        &self.file_preview
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

    /// Decode any `sampler:file:...` sources that are not loaded yet.
    ///
    /// A project saved after a rack drop records the path in the source id.
    /// Opening it has to put the oneshot back before the node is built, or the
    /// track is silent.
    pub fn ensure_file_samplers(&self) {
        let sources: Vec<Source> = self
            .project
            .lock()
            .tracks
            .iter()
            .map(|track| track.source.clone())
            .collect();
        for source in sources {
            self.ensure_file_sampler(&source);
        }
    }

    /// Load a dropped-file sampler if `source` names one that is not in the
    /// registry yet. Failures are logged: the track stays silent, the way a
    /// missing piano does.
    pub fn ensure_file_sampler(&self, source: &Source) {
        let Source::Sampler(reference) = source else {
            return;
        };
        let Some(path) = file_path_of(&reference.instrument_id) else {
            return;
        };
        if self.sampler.contains(&reference.wire_id()) {
            return;
        }
        if let Err(err) = self.sampler.install_file(path) {
            log::warn!("could not reload dropped sample {}: {err}", path.display());
        }
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
        mixer_state_dto(&project, selected, clap, vst3, missing, &self.effects)
    }

    // ------------------------------------------------------- routing edits

    /// Apply a **structural** edit to the project, then republish.
    ///
    /// The graph is *not* rebuilt here: adding a bus, a send or an insert
    /// changes how many nodes a graph has, and only a caller that knows whether
    /// a hosted plugin is loaded can choose between rebuilding inline and going
    /// through a host's main thread. See
    /// [`AppState::rebuild_graph`](crate::AppState::rebuild_graph).
    ///
    /// A refused edit leaves the project exactly as it was, because every
    /// operation this funnels validates before it mutates.
    ///
    /// # Errors
    /// Whatever the edit reported.
    pub fn edit_project<T, E>(
        &self,
        edit: impl FnOnce(&mut Project) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut project = self.project.lock();
        let value = edit(&mut project)?;
        project.publish(&self.params);
        Ok(value)
    }

    /// Set a bus's fader, in dB.
    ///
    /// A store into the parameter block, exactly as a track's fader is: no
    /// rebuild, nothing allocated, heard on the next block with a 10 ms ramp.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`] or [`TrackError::InvalidGain`].
    pub fn set_bus_gain_db(&self, bus: usize, db: f32) -> Result<(), MixerError> {
        let db = check_gain_db(db)?;
        self.edit_project(|project| project.edit_bus(bus, |def| def.gain_db = db))?;
        Ok(())
    }

    /// Set a bus's pan.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`] or [`TrackError::InvalidPan`].
    pub fn set_bus_pan(&self, bus: usize, pan: f32) -> Result<(), MixerError> {
        let pan = check_pan(pan)?;
        self.edit_project(|project| project.edit_bus(bus, |def| def.pan = pan))?;
        Ok(())
    }

    /// Mute or unmute a bus.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`].
    pub fn set_bus_muted(&self, bus: usize, muted: bool) -> Result<(), RouteError> {
        self.edit_project(|project| project.edit_bus(bus, |def| def.muted = muted))
    }

    /// Solo or un-solo a bus. Republishes every strip's effective mute, because
    /// solo is a question about the whole project -- see [`Project`].
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`].
    pub fn set_bus_soloed(&self, bus: usize, soloed: bool) -> Result<(), RouteError> {
        self.edit_project(|project| project.edit_bus(bus, |def| def.soloed = soloed))
    }

    /// Rename a bus. Nothing crosses the RT wall at all.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`] or [`RouteError::BlankName`].
    pub fn rename_bus(&self, bus: usize, name: &str) -> Result<(), RouteError> {
        if name.trim().is_empty() {
            return Err(RouteError::BlankName);
        }
        self.edit_project(|project| project.edit_bus(bus, |def| def.name = name.to_owned()))
    }

    /// Set or clear a bus's colour. A blank string clears it.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`].
    pub fn set_bus_color(&self, bus: usize, color: Option<String>) -> Result<(), RouteError> {
        let color = color.filter(|value| !value.trim().is_empty());
        self.edit_project(|project| project.edit_bus(bus, |def| def.color = color))
    }

    /// Set a send's level. **One atomic store; no rebuild, no allocation.**
    ///
    /// Returns nothing rather than the whole mixer, because a send knob drag
    /// produces one of these per frame and the caller already knows what it
    /// asked for.
    ///
    /// # Errors
    /// [`RouteError::NoSuchSend`] or [`RouteError::InvalidLevel`].
    pub fn set_send_level(&self, id: u32, level: f32) -> Result<(), RouteError> {
        let level = crate::project::check_send_level(level)?;
        let mut project = self.project.lock();
        let mut source = None;
        project.edit_send(id, |send| {
            send.level = level;
            source = Some((send.source, send.dest));
        })?;
        if let Some((source, dest)) = source {
            self.params.set_send_level(source, dest, level);
        }
        Ok(())
    }

    /// Move a send between pre- and post-fader. **One atomic store.**
    ///
    /// # Errors
    /// [`RouteError::NoSuchSend`].
    pub fn set_send_pre(&self, id: u32, pre: bool) -> Result<(), RouteError> {
        let mut project = self.project.lock();
        let mut source = None;
        project.edit_send(id, |send| {
            send.pre = pre;
            source = Some((send.source, send.dest));
        })?;
        if let Some((source, dest)) = source {
            self.params.set_send_pre(source, dest, pre);
        }
        Ok(())
    }

    /// Set one insert parameter, returning **what was actually stored**.
    ///
    /// One atomic store. No rebuild, no allocation, and the answer is the
    /// sanitised value so a knob can snap itself to the truth rather than draw
    /// where the engine is not.
    ///
    /// # Errors
    /// [`InsertError::NoSuchInsert`] and friends, or
    /// [`InsertError::NoSuchParam`] for an id the effect does not have.
    pub fn set_insert_param(
        &self,
        target: InsertTarget,
        index: usize,
        param_id: &str,
        value: f32,
    ) -> Result<f32, InsertError> {
        let project = self.project.lock();
        let def = Self::insert_at(&project, target, index)?;
        def.params
            .set_by_id(param_id, value)
            .ok_or_else(|| InsertError::NoSuchParam {
                effect: def.type_id.clone(),
                param: param_id.to_owned(),
            })
    }

    /// One insert's live meter values.
    ///
    /// # Errors
    /// As [`set_insert_param`](Self::set_insert_param), minus the parameter.
    pub fn insert_readouts(
        &self,
        target: InsertTarget,
        index: usize,
    ) -> Result<Vec<crate::effects::ParamValueDto>, InsertError> {
        let project = self.project.lock();
        let def = Self::insert_at(&project, target, index)?;
        Ok(def
            .params
            .readout_snapshot()
            .into_iter()
            .map(|(id, value)| crate::effects::ParamValueDto {
                id: id.to_owned(),
                value,
            })
            .collect())
    }

    /// Every meter on one chain, in chain order.
    ///
    /// # Errors
    /// [`InsertError::NoSuchTrack`] / [`InsertError::NoSuchBus`].
    pub fn chain_readouts(
        &self,
        target: InsertTarget,
    ) -> Result<Vec<crate::effects::InsertReadoutsDto>, InsertError> {
        let project = self.project.lock();
        let chain = project
            .chain(target)
            .ok_or(InsertError::no_such_chain(target))?;
        Ok(crate::effects::chain_readouts_dto(chain))
    }

    /// EQ hold-to-solo of the source. `band` is a prefix (`hp`, `bell1`, …), or
    /// `None` to stop. A no-op on a non-EQ insert.
    ///
    /// # Errors
    /// As [`set_insert_param`](Self::set_insert_param), minus the parameter.
    pub fn set_insert_listen(
        &self,
        target: InsertTarget,
        index: usize,
        band: Option<&str>,
    ) -> Result<(), InsertError> {
        let project = self.project.lock();
        let def = Self::insert_at(&project, target, index)?;
        let value = match band {
            None => -1,
            Some(id) => mydaw_plugin_effects::eq::BAND_IDS
                .iter()
                .position(|&name| name == id)
                .map(|i| i as i32)
                .unwrap_or(-1),
        };
        def.listen
            .store(value, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn insert_at(
        project: &Project,
        target: InsertTarget,
        index: usize,
    ) -> Result<&InsertDef, InsertError> {
        let chain = project
            .chain(target)
            .ok_or(InsertError::no_such_chain(target))?;
        chain
            .get(index)
            .ok_or(InsertError::NoSuchInsert { target, index })
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

    /// Channel pitch for an audio-clip or one-shot track, in cents.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`]. Out-of-range values are clamped to ±1200.
    pub fn set_pitch_cents(&self, track: usize, cents: f32) -> Result<(), TrackError> {
        let cents = if cents.is_finite() {
            cents.clamp(-1_200.0, 1_200.0)
        } else {
            0.0
        };
        self.edit(track, |def| {
            def.pitch_cents = cents;
            def.sample_play.set_pitch_cents(cents);
        })
    }

    /// Fade in and out for an audio-clip or one-shot track, in milliseconds.
    ///
    /// Negative and non-finite values become zero rather than an error: this is
    /// reachable from a webview knob, and "no fade" is the only sane reading of
    /// a fade of NaN.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_fades(
        &self,
        track: usize,
        fade_in_ms: f32,
        fade_out_ms: f32,
    ) -> Result<(), TrackError> {
        let sane = |ms: f32| {
            if ms.is_finite() {
                ms.clamp(0.0, MAX_FADE_MS)
            } else {
                0.0
            }
        };
        let (fade_in, fade_out) = (sane(fade_in_ms), sane(fade_out_ms));
        self.edit(track, |def| {
            def.fade_in_ms = fade_in;
            def.fade_out_ms = fade_out;
            def.sample_play.set_fades(fade_in, fade_out);
        })
    }

    /// Playback root for an audio-clip or one-shot track.
    ///
    /// Updates the scalar project fields and the shared RT atomics without
    /// replacing the sounding node, so a held note keeps going.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], [`TrackError::InvalidRootKey`], or
    /// [`TrackError::InvalidRootFine`].
    pub fn set_sample_root(
        &self,
        track: usize,
        key: u8,
        fine_cents: f32,
        mode: crate::project::RootMode,
    ) -> Result<(), TrackError> {
        let key = crate::project::check_root_key(key)?;
        let fine = crate::project::check_root_fine(fine_cents)?;
        self.edit(track, |def| {
            def.root_key = key;
            def.root_fine_cents = fine;
            def.root_mode = mode;
            if mode == crate::project::RootMode::Detected {
                let previous = def.detected_pitch;
                def.detected_pitch = Some(crate::project::RootPitchDto {
                    midi_note: key,
                    cents: fine,
                    confidence: previous.map(|pitch| pitch.confidence).unwrap_or(1.0),
                    voiced_coverage: previous.map(|pitch| pitch.voiced_coverage).unwrap_or(1.0),
                    source: previous
                        .map(|pitch| pitch.source)
                        .unwrap_or(crate::project::PitchSourceDto::Analysis),
                });
            }
            def.sample_play.set_root(key, fine);
        })
    }

    /// Remember the last successful pitch detection on this channel.
    ///
    /// Does not change playback root. [`set_sample_root`](Self::set_sample_root)
    /// is what calibrates the keyboard.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_detected_pitch(
        &self,
        track: usize,
        pitch: Option<crate::project::RootPitchDto>,
    ) -> Result<(), TrackError> {
        self.edit(track, |def| def.detected_pitch = pitch)
    }

    /// SMP region for an audio-clip or one-shot track, in the file's frames.
    ///
    /// `end == 0` means the file's last frame.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_sample_region(
        &self,
        track: usize,
        start_frames: u64,
        end_frames: u64,
    ) -> Result<(), TrackError> {
        self.edit(track, |def| {
            def.sample_start_frames = start_frames;
            def.sample_end_frames = end_frames;
            def.sample_play.set_region(start_frames, end_frames);
        })
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
        self.build_node_with_params(source, None, sample_rate, max_block)
    }

    /// The same, against a parameter block the caller owns.
    ///
    /// `None` builds the instrument at its defaults, which is what an offline
    /// preview wants: it renders one note of a sound nobody has tweaked yet.
    ///
    /// # Errors
    /// [`SourceError::MissingInstrument`] for a `type_id` this build lacks.
    pub fn build_node_with_params(
        &self,
        source: &Source,
        params: Option<Arc<mydaw_rt_core::ParamBlock>>,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Box<dyn RtNode>, SourceError> {
        match source {
            Source::TestTone => Ok(self.tone.create(sample_rate, max_block)),
            Source::Clap(_) | Source::Vst3(_) | Source::AudioClip { .. } => {
                Ok(Box::new(SilentNode))
            }
            Source::OneShot { source_id } => {
                // The registry-backed player, whole file at recorded pitch, so
                // an offline render (the channel window's preview) makes sound.
                // The graph builders pass the track's crop and pitch to the
                // same node themselves; this path has no track to read.
                Ok(self.audio_clips.oneshot_node(
                    *source_id,
                    Arc::new(SamplePlaybackParams::new(0, 0, 0.0, (0.0, 0.0))),
                ))
            }
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
                match params {
                    Some(block) => {
                        self.instruments
                            .create_with_params(type_id, sample_rate, max_block, block)
                    }
                    None => self.instruments.create(type_id, sample_rate, max_block),
                }
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
        self.ensure_file_sampler(&source);
        let info = audio.stream_info().ok_or(AudioIoError::NotRunning)?;
        let sample_play = {
            let project = self.project.lock();
            let def = project
                .tracks
                .get(track)
                .ok_or(TrackError::NoSuchTrack(track))?;
            Arc::clone(&def.sample_play)
        };
        // The new instrument's knobs, created *before* its node so the two share
        // one block: the node reads what the shell writes, and the shell writes
        // what a project file restores.
        let params = TrackDef::params_for(&source);
        let node = match &source {
            Source::AudioClip { source_id } => self.audio_clips.open_for_source(
                *source_id,
                Arc::clone(&sample_play),
                audio.gc_handle(),
            ),
            Source::OneShot { source_id } => self.audio_clips.oneshot_node(*source_id, sample_play),
            other => self.build_node_with_params(
                other,
                params.clone(),
                f64::from(info.sample_rate),
                info.max_block,
            )?,
        };
        self.install_source_with_params(track, source, params, node)?;
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

        let params = TrackDef::params_for(&source);
        self.record_source(track, source, params)
    }

    /// The same, for a caller that already built the node against a block.
    ///
    /// Which is what [`set_source`](Self::set_source) does, and why it cannot
    /// use the plain version: the block the node is reading and the block the
    /// track hands the UI have to be the *same* one, and a second
    /// `params_for` would make two.
    pub fn install_source_with_params(
        &self,
        track: usize,
        source: Source,
        params: Option<Arc<mydaw_rt_core::ParamBlock>>,
        node: Box<dyn RtNode>,
    ) -> Result<(), MixerError> {
        let handle = self
            .handles
            .get(track)
            .ok_or(TrackError::NoSuchTrack(track))?;
        handle.install(node, source.clone())?;
        self.record_source(track, source, params)
    }

    /// Point a **sample** channel at a different loaded file.
    ///
    /// Narrower than [`set_source`](Self::set_source) on purpose. That call
    /// swaps an instrument for another instrument and needs the open stream's
    /// rate and block size to build one, so it refuses before it changes
    /// anything when no device is open. Repointing a one-shot or clip channel
    /// needs neither: the node reads a decoded buffer, not a device. Making a
    /// destructive sample edit go through `set_source` would mean the edit
    /// wrote a file and then silently did not apply on a machine with no sound
    /// card -- exactly the half-done state this codebase refuses elsewhere.
    ///
    /// So the project moves first and the node follows if there is somewhere to
    /// send it. `Err(AudioIoError::NotRunning)` therefore means "recorded, not
    /// yet audible", which is the contract `publish_song` already has.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], [`TrackError::NotASampleChannel`], or
    /// [`AudioIoError::NotRunning`] after the project was already updated.
    pub fn repoint_sample_source(
        &self,
        track: usize,
        source_id: u64,
        audio: &AudioIoService,
    ) -> Result<(), MixerError> {
        let source = {
            let mut project = self.project.lock();
            let def = project
                .tracks
                .get_mut(track)
                .ok_or(TrackError::NoSuchTrack(track))?;
            let source = match def.source {
                Source::AudioClip { .. } => Source::AudioClip { source_id },
                Source::OneShot { .. } => Source::OneShot { source_id },
                _ => return Err(TrackError::NotASampleChannel(track).into()),
            };
            def.source = source.clone();
            source
        };
        let sample_play = {
            let project = self.project.lock();
            Arc::clone(&project.tracks[track].sample_play)
        };
        let node: Box<dyn RtNode> = match &source {
            Source::AudioClip { source_id } => self.audio_clips.open_for_source(
                *source_id,
                sample_play,
                audio.gc_handle(),
            ),
            _ => self.audio_clips.oneshot_node(source_id, sample_play),
        };
        let handle = self
            .handles
            .get(track)
            .ok_or(TrackError::NoSuchTrack(track))?;
        handle.install(node, source)?;
        Ok(())
    }

    fn record_source(
        &self,
        track: usize,
        source: Source,
        params: Option<Arc<mydaw_rt_core::ParamBlock>>,
    ) -> Result<(), MixerError> {
        let mut project = self.project.lock();
        let Some(def) = project.tracks.get_mut(track) else {
            return Err(TrackError::NoSuchTrack(track).into());
        };
        def.source = source;
        def.source_params = params;
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
        let selected_before = self.selected.load(Ordering::Relaxed);
        {
            let mut project = self.project.lock();
            project.remove(track)?;
            project.publish(&self.params);
        }
        // Keep the same instrument selected when a lower row disappears. If
        // the selected instrument itself was removed, choose its predecessor
        // rather than silently jumping to the row that moved underneath it.
        let selected_after = if selected_before == track {
            track.saturating_sub(1)
        } else if selected_before > track {
            selected_before - 1
        } else {
            selected_before
        };
        self.selected.store(selected_after, Ordering::Relaxed);
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
        self.ensure_file_samplers();
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
        self.assemble(limits, gc, None)
    }

    /// The same graph, wired to **nothing the live stream owns**.
    ///
    /// [`build_spec`](Self::build_spec) hands each track's head to the session's
    /// own [`SourceHandle`], which is how a later instrument swap reaches the
    /// audio thread. Building it a second time for an offline render would
    /// therefore repoint every one of those handles at a graph nobody is
    /// listening to: the stream would keep playing, and the next knob or source
    /// change would land in a queue with no consumer.
    ///
    /// So an offline render gets its own handles. They are returned in
    /// [`OfflineGraph::handles`] and must outlive the render — dropping a
    /// `SourceHandle` abandons its queue, and an abandoned queue is a slot that
    /// stops accepting the node it already has.
    ///
    /// What an offline graph does **not** carry, deliberately:
    ///
    /// - the **file-preview** node, which is the browser's audition player. A
    ///   render is of the project, not of whatever the user last clicked in the
    ///   file list.
    /// - a **hosted CLAP or VST3** plugin. `build_spec` cannot carry one either
    ///   (only the main thread may instantiate one, and it re-attaches them
    ///   after the graph is installed), so an offline render is of the built-in
    ///   chain. The caller says so rather than pretending otherwise.
    pub fn build_offline_spec(
        &self,
        limits: &mydaw_rt_core::GraphLimits,
        gc: Handle,
    ) -> OfflineGraph {
        let mut handles = Vec::new();
        let spec = self.assemble(limits, gc, Some(&mut handles));
        OfflineGraph { spec, handles }
    }

    /// The body of both. `offline` present means "detach from the session".
    fn assemble(
        &self,
        limits: &mydaw_rt_core::GraphLimits,
        gc: Handle,
        mut offline: Option<&mut Vec<Arc<SourceHandle>>>,
    ) -> GraphSpec {
        let live = offline.is_none();
        let project = self.project.lock();
        project.publish(&self.params);

        if live {
            self.audio_clips.begin_rebuild();
        }

        let mut spec = GraphSpec::new()
            .with_params(Arc::clone(&self.params))
            .with_limits(*limits);

        for (index, def) in project.tracks.iter().enumerate() {
            let node = match &def.source {
                Source::AudioClip { source_id } if live => self.audio_clips.open_for_source(
                    *source_id,
                    Arc::clone(&def.sample_play),
                    gc.clone(),
                ),
                Source::AudioClip { source_id } => self.audio_clips.detached_for_source(
                    *source_id,
                    Arc::clone(&def.sample_play),
                    gc.clone(),
                ),
                Source::OneShot { source_id } => self
                    .audio_clips
                    .oneshot_node(*source_id, Arc::clone(&def.sample_play)),
                // The track's own block, so a rebuilt graph picks the synth up
                // where the last one left it -- the argument `MixerParams`
                // makes for faders, at knob granularity.
                other => match self.build_node_with_params(
                    other,
                    def.source_params.clone(),
                    limits.sample_rate,
                    limits.max_block,
                ) {
                    Ok(node) => node,
                    Err(err) => {
                        log::warn!(
                            "track {index} ('{}'): {err}; falling back to the test tone",
                            def.name
                        );
                        self.tone.create(limits.sample_rate, limits.max_block)
                    }
                },
            };
            let (handle, gate) = match offline.as_deref_mut() {
                None => (Arc::clone(&self.handles[index]), Arc::clone(&self.gates[index])),
                Some(held) => {
                    let handle = Arc::new(SourceHandle::new(def.source.clone()));
                    held.push(Arc::clone(&handle));
                    (handle, Arc::new(AtomicBool::new(false)))
                }
            };
            let slot = handle.open(node, def.source.clone(), gc.clone(), Arc::clone(&gate));
            spec.push_track(track_chain(
                slot,
                self.build_chain(&def.inserts, limits),
                gate,
            ));
        }

        for def in &project.buses {
            let mut bus = BusSpec::new();
            bus.inserts = self.build_chain(&def.inserts, limits);
            spec.push_bus(bus);
        }

        for send in &project.sends {
            spec.push_send(SendSpec::new(send.source, send.dest));
        }

        if live {
            spec.push_master_insert(crate::file_preview::file_preview_node(
                &self.file_preview,
                gc.clone(),
            ));
        }

        // The master's *internal* chain runs first and the one hosted insert
        // last. That order keeps the hosted slot a single unconditional node at
        // a fixed position, which is what makes its rebuild-free swap work (see
        // `crate::insert`) -- and it is the order a user gets anyway by
        // inserting the built-ins and then reaching for a plugin to finalise the
        // bus.
        for node in self.build_chain(&project.master_inserts, limits) {
            spec.push_master_insert(node);
        }
        if live {
            spec.push_master_insert(crate::insert::insert_slot(&self.master_insert, gc));
        }
        spec
    }

    /// Instantiate one insert chain against the blocks the project owns.
    ///
    /// Every node is handed the **same** `Arc<ParamBlock>` the session holds, so
    /// a rebuild picks each effect up exactly where the last graph left it --
    /// the argument [`MixerParams`] makes for faders, at knob granularity.
    ///
    /// An insert naming an effect this build does not have is **skipped with a
    /// warning**, not fatal: that is the missing-plugin policy one level down,
    /// and the entry stays in the project so a save writes it back out.
    fn build_chain(
        &self,
        chain: &[InsertDef],
        limits: &mydaw_rt_core::GraphLimits,
    ) -> Vec<Box<dyn RtNode>> {
        chain
            .iter()
            .filter_map(|def| {
                let node = self.effects.create_in_chain(
                    &def.type_id,
                    limits.sample_rate,
                    limits.max_block,
                    Arc::clone(&def.params),
                    Arc::clone(&def.listen),
                );
                if node.is_none() {
                    log::warn!(
                        "'{}' is not an effect this build has; skipping it in the chain",
                        def.type_id
                    );
                }
                node
            })
            .collect()
    }

    /// Forget every track's stream channel, and the master insert's. Called when
    /// audio stops.
    pub fn close(&self) {
        for handle in &self.handles {
            handle.close();
        }
        self.master_insert.close();
        self.audio_clips.close();
        self.file_preview.close();
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
            Arc::new(EffectRegistry::new()),
        )
    }

    #[test]
    fn a_fresh_host_publishes_the_default_project_before_anything_asks() {
        // The parameter block has to be right from the first block the audio
        // thread runs, which is before any command has arrived.
        let host = host();
        let count = Project::default().len();
        assert_eq!(host.track_count(), count);
        assert_eq!(host.selected(), 0);
        assert_eq!(
            host.selected_source(),
            Source::Drum(mydaw_plugin_instruments::SingleDrum::Kick)
        );

        let params = host.params();
        assert_eq!(params.track_gain(0), 1.0);
        assert!((0..count).all(|index| !params.track_muted(index)));
        assert!(
            params.track_muted(count),
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
        assert_eq!(
            host.selected_source(),
            Source::Drum(mydaw_plugin_instruments::SingleDrum::Kick)
        );
        host.set_selected(1).unwrap();
        assert_eq!(host.selected(), 1);
        assert_eq!(
            host.selected_source(),
            Source::Drum(mydaw_plugin_instruments::SingleDrum::Snare)
        );
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
    fn the_graph_the_host_builds_is_one_track_chain_per_track_plus_the_master_bus() {
        let host = host();
        let collector = mydaw_rt_core::basedrop::Collector::new();
        let spec = host.build_spec(&LIMITS, collector.handle());

        let count = Project::default().len();
        assert_eq!(spec.track_count(), count);
        // Source slot plus transport gate per track, plus the two master-bus
        // nodes: the file-preview player and the master insert slot.
        assert_eq!(spec.len(), count * 2 + 2);
        assert!(spec.params().is_some(), "the mixer parameters are shared");
        assert_eq!(spec.limits(), LIMITS);
        assert!(!host.has_clap_node());
        assert!(
            host.master_insert().is_open() && !host.master_insert().is_occupied(),
            "the slot is there, and empty"
        );
        assert!(
            !host.audio_clips().is_open(),
            "clip nodes are only compiled onto Audio Clip channels"
        );
    }

    #[test]
    fn an_audio_clip_channel_opens_a_filtered_clip_node() {
        let host = host();
        let mut project = host.project();
        project
            .add(Source::AudioClip { source_id: 7 }, Some("Kick.wav".into()))
            .unwrap();
        host.set_project(project);
        let collector = mydaw_rt_core::basedrop::Collector::new();
        let _ = host.build_spec(&LIMITS, collector.handle());
        assert!(host.audio_clips().is_open());
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
        assert_eq!(
            host.track_count(),
            Project::default().len() + 1,
            "the project edit happened"
        );
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
        let added = Project::default().len();
        assert_eq!(host.track_count(), added + 1);
        assert!(
            !host.params().track_muted(added),
            "the new track is published"
        );
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

    #[test]
    fn removing_tracks_preserves_selection_by_instrument_and_prefers_the_previous_row() {
        let host = host();
        let audio = mydaw_plugin_audio_io::AudioIoService::default();
        let _ = host.add_track(Source::TestTone, Some("Two".to_owned()), &audio);
        let _ = host.add_track(Source::TestTone, Some("Three".to_owned()), &audio);

        host.set_selected(2).unwrap();
        let _ = host.remove_track(0, &audio);
        assert_eq!(host.selected(), 1, "the same instrument moved up one row");

        let _ = host.remove_track(1, &audio);
        assert_eq!(
            host.selected(),
            0,
            "a removed selection falls back to the row above"
        );
    }
}
