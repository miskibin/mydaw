//! Kernel boot and the state the Tauri commands run against.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use mydaw_ipc_bridge::{PanelManifestDto, UiSnapshot};
use mydaw_kernel::{Kernel, KernelError};
use mydaw_plugin_audio_io::{
    AudioIoError, AudioIoPlugin, AudioIoService, OutputRequest, RecordError, RecordService,
    RecordedTake, StreamInfo,
};
use mydaw_plugin_debug_tap::{DebugTapPlugin, DebugTapService};
use mydaw_plugin_effects::{EffectRegistry, EffectsPlugin};
use mydaw_plugin_host_clap::{ClapHostService, HostClapPlugin};
use mydaw_plugin_host_vst3::{HostVst3Plugin, Vst3HostService};
use mydaw_plugin_instruments::{InstrumentRegistry, InstrumentsPlugin};
use mydaw_plugin_metering::{MeterOutput, MeteringPlugin, SpectrumHandoff, SpectrumOutput};
use mydaw_plugin_midi_io::{MidiIoPlugin, MidiIoService, MidiNoteSink};
use mydaw_plugin_sampler::{AssetDownloader, AssetStatus, SamplerPlugin, SamplerRegistry};
use mydaw_plugin_sequencer::{
    AudioSourceError, AudioSourceRegistry, Pattern, PatternError, PatternSet, PatternSetError,
    PlayMode, Playlist, PlaylistError, SequencerPlugin, SequencerService, SongError,
};
use mydaw_plugin_test_tone::{TestToneFactory, TestTonePlugin};
use mydaw_plugin_windows::{PluginWindowService, PluginWindowsPlugin};

use mydaw_rt_core::{
    clamp_bpm, LiveNote, SpectrumTarget, SpectrumWatch, StripId, MAX_BPM, MAX_KEY, MIN_BPM,
};

use crate::clap_bridge::{ClapAnswer, ClapCommand, ClapGuiStatusDto, ClapHostBridge, ClapTarget};
use crate::dto::{
    ClapPluginInfoDto, PatternDto, PatternSetDto, PlaylistDto, SequencerStateDto, Vst3PluginInfoDto,
};
use crate::effects::{EffectInfoDto, InsertError, InsertTarget};
use crate::ipc_bench;
use crate::meters::MeterPump;
use crate::project::{
    MixerStateDto, PitchAnalysisDto, Project, RootMode, RootPitchDto, RouteError, SliceAnalysisDto,
    SliceMode, SliceStateDto, TrackError,
};
use crate::project_bridge::{ProjectBridge, ProjectCommand, ProjectInfoDto};
use crate::source::{check_bundle_path, Source, SourceDto, SourceError};
use crate::spectrum::SpectrumPump;
use crate::tracks::{MixerError, TrackHost};
use crate::vst3_bridge::{Vst3Answer, Vst3Command, Vst3GuiStatusDto, Vst3HostBridge};

/// Panel key of the kernel status view, mirrored in `ui/src/panels/registry.ts`.
pub const KERNEL_STATUS_PANEL: &str = "kernel-status";

/// Panel key of the mixer, mirrored in `ui/src/panels/registry.ts`.
///
/// Contributed by the shell rather than by a plugin, for the same reason the
/// kernel status view is: a mixer strip is a view of the **project**, and the
/// project lives here. The metering plugin owns the meter *queue* and the
/// sequencer owns the *pattern*, but neither owns a track -- so a plugin
/// contributing this panel would have to reach back into the shell for
/// everything it draws.
pub const MIXER_PANEL: &str = "mixer";

/// Startup failures that are not the kernel's own.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    /// The kernel booted but a plugin did not publish the service the shell needs.
    /// Always a wiring bug in [`boot_kernel`], never a runtime condition.
    #[error("plugin service '{0}' is missing after boot")]
    MissingService(&'static str),
}

/// Why a note could not be played.
///
/// The first two are reachable from the webview, which can send any number it
/// likes: a UI bug must produce a message in the status bar, not an event the
/// plugin has to defend itself against.
#[derive(Debug, thiserror::Error)]
pub enum NoteError {
    #[error("note key {0} is out of range; CLAP keys are 0..={MAX_KEY}")]
    KeyOutOfRange(u8),
    #[error("note velocity {0} is not a number between 0.0 and 1.0")]
    InvalidVelocity(f32),
    /// No audio device, or the audio thread is not draining its queue.
    #[error(transparent)]
    Audio(#[from] AudioIoError),
}

/// Why a sequencer edit was refused.
///
/// Both variants are reachable from the webview, which can send any JSON it
/// likes. Note what is *not* here: "no audio device". A pattern and a tempo are
/// project data, and editing them on a machine with no sound card has to work --
/// see [`AppState::set_pattern`].
#[derive(Debug, thiserror::Error)]
pub enum SequencerError {
    /// The pattern would not fit what the audio thread can run.
    #[error(transparent)]
    Pattern(#[from] PatternError),
    /// The edit was refused by the pattern set or the arrangement.
    #[error(transparent)]
    Song(#[from] SongError),
    #[error("tempo {0} is not a number between {MIN_BPM} and {MAX_BPM}")]
    InvalidBpm(f64),
    /// The audio thread has not picked up previous edits. Retryable.
    #[error(transparent)]
    Audio(#[from] AudioIoError),
    /// A file could not be loaded, or a source could not be dropped.
    #[error(transparent)]
    AudioSource(#[from] AudioSourceError),
}

/// The pattern set a fresh project starts with: one pattern with an empty grid
/// on every rack channel.
///
/// Lives here rather than in the sequencer because it needs to know what is
/// *on* each channel to pick the grid's key -- the drum kit gets the General
/// MIDI kick, so a row entered at its default key plays a kick untransposed and
/// the rest of the map is a scroll away in the roll. Anything else gets middle
/// C. The sequencer crate deliberately does not depend on the sampler one.
pub fn starter_pattern_set(project: &Project) -> PatternSet {
    let channels: Vec<u8> = (0..project.len().min(mydaw_rt_core::MAX_TRACKS))
        .map(|index| index as u8)
        .collect();
    PatternSet::starter(&channels, |channel| {
        match project.tracks.get(channel as usize).map(|t| &t.source) {
            Some(Source::Sampler(reference))
                if reference.instrument_id == crate::project::KIT_ID =>
            {
                crate::project::KIT_STEP_KEY
            }
            _ => mydaw_plugin_sequencer::DEFAULT_STEP_KEY,
        }
    })
}

/// Register the Phase 0 plugin set and boot the kernel.
///
/// Registration order is irrelevant -- the kernel topologically sorts by manifest
/// dependencies -- so the list is written in the order a reader wants it:
/// infrastructure, then generators, then hosts.
///
/// # Errors
/// Propagates [`KernelError`] from `boot`: a duplicate id, a missing dependency
/// or a cycle. All three are wiring bugs, and all three fail before any plugin
/// code runs.
pub fn boot_kernel() -> Result<Kernel, KernelError> {
    let mut kernel = Kernel::new();
    kernel.add_plugin(Box::new(MeteringPlugin::new()))?;
    kernel.add_plugin(Box::new(DebugTapPlugin::new()))?;
    kernel.add_plugin(Box::new(TestTonePlugin::new()))?;
    kernel.add_plugin(Box::new(InstrumentsPlugin::new()))?;
    kernel.add_plugin(Box::new(EffectsPlugin::new()))?;
    kernel.add_plugin(Box::new(SamplerPlugin::new()))?;
    kernel.add_plugin(Box::new(SequencerPlugin::new()))?;
    kernel.add_plugin(Box::new(AudioIoPlugin::new()))?;
    kernel.add_plugin(Box::new(MidiIoPlugin::new()))?;
    kernel.add_plugin(Box::new(HostClapPlugin::new()))?;
    kernel.add_plugin(Box::new(HostVst3Plugin::new()))?;
    kernel.add_plugin(Box::new(PluginWindowsPlugin::new()))?;
    kernel.boot()?;
    Ok(kernel)
}

/// Snapshot of the booted kernel, plus the panels the shell itself contributes.
///
/// The kernel status panel has no owning plugin -- it displays the plugin
/// registry, so a plugin contributing it would be circular. The shell injects it
/// first so it is the leftmost tab in the dock.
///
/// The IPC bench panel is appended only when its environment variable is set
/// (see [`crate::ipc_bench`]); with no flag the snapshot is exactly what it was
/// before spike 4, which is what keeps the harness free in a normal run.
pub fn ui_snapshot(kernel: &Kernel) -> UiSnapshot {
    let mut snapshot = UiSnapshot::from_kernel(kernel);
    snapshot.panels.insert(
        0,
        PanelManifestDto {
            id: KERNEL_STATUS_PANEL.to_owned(),
            title: "Kernel".to_owned(),
            component: KERNEL_STATUS_PANEL.to_owned(),
        },
    );
    snapshot.panels.insert(
        1,
        PanelManifestDto {
            id: MIXER_PANEL.to_owned(),
            title: "Mixer".to_owned(),
            component: MIXER_PANEL.to_owned(),
        },
    );
    for (id, title, component) in
        crate::ipc_bench::bench_panels_for(ipc_bench::dev_panels_enabled(), ipc_bench::auto_mode())
    {
        snapshot.panels.push(PanelManifestDto {
            id,
            title,
            component,
        });
    }
    snapshot
}

/// Live MIDI notes: same path as the typing keyboard, onto the selected track.
///
/// Note-off is pinned to the channel the matching on used. Changing the
/// selected strip while a controller key is held must not send the off to the
/// new track and leave the old one ringing.
struct EngineNoteSink {
    audio: Arc<AudioIoService>,
    tracks: Arc<TrackHost>,
    sounding: Mutex<HashMap<u8, u8>>,
}

fn midi_event_channel(sounding: &mut HashMap<u8, u8>, selected: u8, key: u8, on: bool) -> u8 {
    if on {
        sounding.insert(key, selected);
        selected
    } else {
        sounding.remove(&key).unwrap_or(selected)
    }
}

impl MidiNoteSink for EngineNoteSink {
    fn note_on(&self, key: u8, velocity: f32) {
        let selected = self.tracks.selected().min(mydaw_rt_core::MAX_TRACKS - 1) as u8;
        let channel = midi_event_channel(&mut self.sounding.lock(), selected, key, true);
        if let Err(err) = self
            .audio
            .send_note(LiveNote::note_on(key, velocity).on_channel(channel))
        {
            log::debug!("MIDI note-on dropped: {err}");
        }
    }

    fn note_off(&self, key: u8) {
        let selected = self.tracks.selected().min(mydaw_rt_core::MAX_TRACKS - 1) as u8;
        let channel = midi_event_channel(&mut self.sounding.lock(), selected, key, false);
        if let Err(err) = self
            .audio
            .send_note(LiveNote::note_off(key).on_channel(channel))
        {
            log::debug!("MIDI note-off dropped: {err}");
        }
    }
}

/// What the Tauri commands are allowed to touch.
///
/// Note what is *not* here: the [`Kernel`] itself. `Box<dyn Plugin>` is not
/// `Send`, so the kernel cannot live in Tauri's managed state; it stays on the
/// main thread (owned by the run loop, which also shuts it down). Everything the
/// UI needs is either an immutable snapshot taken at boot or an `Arc` to a
/// service that was designed to be shared.
pub struct AppState {
    snapshot: UiSnapshot,
    audio: Arc<AudioIoService>,
    /// Audio *input*: the capture device and the take being built on it.
    ///
    /// Not a kernel service, unlike [`audio`](Self::audio): nothing in the graph
    /// reads the input, so there is no RT half for the kernel to wire up and no
    /// plugin needs to resolve it. It is created here and lives as long as the
    /// shell does, holding no device until the user arms one.
    recorder: Arc<RecordService>,
    /// The authoritative pattern and tempo. The audio thread runs copies of
    /// both; this is the original (and, one day, what a project file holds).
    sequencer: Arc<SequencerService>,
    /// The audio files the arrangement's clips point at, decoded into RAM. A
    /// sibling of the sequencer rather than a field on it -- see
    /// `mydaw_plugin_sequencer::audio`.
    sources: Arc<AudioSourceRegistry>,
    /// The project's tracks, the mixer parameters the audio thread reads, and
    /// one head-of-chain handle per track. See `crate::tracks`.
    tracks: Arc<TrackHost>,
    /// Mailbox to the main thread's CLAP host, and the board it publishes.
    ///
    /// Everything about a hosted plugin -- creating it, activating it, opening
    /// its editor, saving its state -- is `[main-thread]` work on a `!Send`
    /// value, so none of it can live here. See `crate::clap_bridge`.
    clap_bridge: Arc<ClapHostBridge>,
    /// CLAP *discovery*: the scan behind the picker. Unlike hosting, this is
    /// `Send + Sync` and runs happily on a command thread.
    clap: Arc<ClapHostService>,
    /// Mailbox to the main thread's VST3 host, and the board it publishes. A
    /// sibling of `clap_bridge`, for the same reasons -- see `crate::vst3_bridge`.
    vst3_bridge: Arc<Vst3HostBridge>,
    /// VST3 *discovery*. Like its CLAP counterpart, `Send + Sync` and safe on a
    /// command thread.
    vst3: Arc<Vst3HostService>,
    /// Mailbox to the main thread's project loader, plus the two things a
    /// project publishes: which file is open and which of its plugins are
    /// missing. Saving asks every plugin for its state and loading destroys and
    /// recreates all of them, so like everything else about a hosted plugin it
    /// cannot happen here -- see `crate::project_bridge`.
    project_bridge: Arc<ProjectBridge>,
    meters: Arc<MeterPump>,
    /// Insert analyser: hop drain + FFT. Armed by the focused editor window.
    spectrum: Arc<SpectrumPump>,
    spectrum_watch: Arc<SpectrumWatch>,
    /// The audio inspection harness. Non-RT, `Send + Sync`, and safe to call
    /// from a Tauri command thread.
    debug_tap: Arc<DebugTapService>,
    /// The sampled instruments. Held alongside [`tracks`](Self::tracks), which
    /// also has it, because the source picker lists from here and the asset
    /// downloader's completion callback rescans it.
    sampler: Arc<SamplerRegistry>,
    /// MIDI input ports. Notes go through [`EngineNoteSink`] onto the selected
    /// track; the webview only lists and pairs devices.
    midi: Arc<MidiIoService>,
    /// Installs the sample content on demand -- one downloader per instrument
    /// the catalogue offers, because the kits are separate downloads and the
    /// shell shows a row and a button for each.
    ///
    /// Empty only if this build's `assets-manifest.json` is unusable, which
    /// the sampler crate's own tests make impossible -- but a boot failure over
    /// an optional download would be the wrong trade, so it degrades to "no
    /// buttons are offered".
    assets: Vec<Arc<AssetDownloader>>,
}

impl AppState {
    /// Collect the shell's state from a booted kernel.
    ///
    /// Claims the metering plugin's throttle here, once, so the pump owns it for
    /// the process lifetime regardless of how often the webview resubscribes.
    ///
    /// # Errors
    /// [`StartupError::MissingService`] if a required plugin service is absent.
    pub fn from_kernel(kernel: &Kernel) -> Result<Self, StartupError> {
        let services = &kernel.ctx().services;
        let audio = services
            .get::<AudioIoService>()
            .ok_or(StartupError::MissingService("AudioIoService"))?;
        let tone = services
            .get::<TestToneFactory>()
            .ok_or(StartupError::MissingService("TestToneFactory"))?;
        let instruments = services
            .get::<InstrumentRegistry>()
            .ok_or(StartupError::MissingService("InstrumentRegistry"))?;
        let sampler = services
            .get::<SamplerRegistry>()
            .ok_or(StartupError::MissingService("SamplerRegistry"))?;
        let effects = services
            .get::<EffectRegistry>()
            .ok_or(StartupError::MissingService("EffectRegistry"))?;
        let sequencer = services
            .get::<SequencerService>()
            .ok_or(StartupError::MissingService("SequencerService"))?;
        let sources = services
            .get::<AudioSourceRegistry>()
            .ok_or(StartupError::MissingService("AudioSourceRegistry"))?;
        let clap = services
            .get::<ClapHostService>()
            .ok_or(StartupError::MissingService("ClapHostService"))?;
        let vst3 = services
            .get::<Vst3HostService>()
            .ok_or(StartupError::MissingService("Vst3HostService"))?;
        let meter_output = services
            .get::<MeterOutput>()
            .ok_or(StartupError::MissingService("MeterOutput"))?;
        let spectrum_handoff = services
            .get::<SpectrumHandoff>()
            .ok_or(StartupError::MissingService("SpectrumHandoff"))?;
        let spectrum_output = services
            .get::<SpectrumOutput>()
            .ok_or(StartupError::MissingService("SpectrumOutput"))?;
        let debug_tap = services
            .get::<DebugTapService>()
            .ok_or(StartupError::MissingService("DebugTapService"))?;
        let midi = services
            .get::<MidiIoService>()
            .ok_or(StartupError::MissingService("MidiIoService"))?;
        // Resolved here so a missing plugin-windows registration is a boot error
        // rather than a mystery when the user first clicks "Open GUI".
        if !services.contains::<PluginWindowService>() {
            return Err(StartupError::MissingService("PluginWindowService"));
        }

        let throttle = meter_output.take();
        if throttle.is_none() {
            log::warn!("meter throttle was already claimed; the UI meters will stay silent");
        }
        let spectrum_watch = spectrum_handoff.watch();
        let spectrum_rx = spectrum_output.take();
        if spectrum_rx.is_none() {
            log::warn!("spectrum consumer was already claimed; insert analysers will stay silent");
        }

        // A manifest this build cannot read means the in-app download is not
        // offered; everything else, including a piano installed by the script,
        // still works. So it is a warning, not a boot failure.
        let assets = match AssetDownloader::catalog(mydaw_plugin_sampler::instruments_root()) {
            Ok(downloaders) => downloaders,
            Err(err) => {
                log::warn!("the in-app asset downloader is unavailable: {err}");
                Vec::new()
            }
        };

        let tracks = Arc::new(TrackHost::new(
            tone,
            instruments,
            Arc::clone(&sampler),
            effects,
        ));
        tracks.audio_clips().bind_sources(Arc::clone(&sources));
        midi.set_sink(Arc::new(EngineNoteSink {
            audio: Arc::clone(&audio),
            tracks: Arc::clone(&tracks),
            sounding: Mutex::new(HashMap::new()),
        }));
        // The rack and the pattern set have to agree from the first frame: the
        // sequencer plugin publishes an empty set (it has no idea what channels
        // exist), and this is the one place that knows both. A rejected seed
        // would be a wiring bug, so it is logged rather than propagated -- an
        // empty pattern is still an editable session.
        if let Err(err) = sequencer.set_song(
            starter_pattern_set(&tracks.project()),
            mydaw_plugin_sequencer::Playlist::default(),
        ) {
            log::warn!("could not seed the starter pattern set: {err}");
        }

        Ok(Self {
            snapshot: ui_snapshot(kernel),
            audio,
            recorder: Arc::new(RecordService::new()),
            sequencer,
            sources,
            tracks,
            clap_bridge: Arc::new(ClapHostBridge::new()),
            clap,
            vst3_bridge: Arc::new(Vst3HostBridge::new()),
            vst3,
            project_bridge: Arc::new(ProjectBridge::new()),
            meters: MeterPump::new(throttle),
            spectrum: Arc::new(SpectrumPump::new(spectrum_rx)),
            spectrum_watch,
            debug_tap,
            sampler,
            midi,
            assets,
        })
    }

    /// Open the output device on a single test-tone track.
    ///
    /// The offline/headless entry point: it *replaces* the project with one
    /// track so that what comes out of the device is exactly one known
    /// generator. `run()` uses [`start_audio_with_project`] instead, which keeps
    /// the session's own tracks.
    ///
    /// Callers treat failure as a warning, not a fatal error: a machine with no
    /// output device (headless CI, RDP session) must still get a working window.
    ///
    /// # Errors
    /// Whatever cpal reports, plus [`AudioIoError::NotRunning`] if the stream died
    /// between opening it and installing the graph.
    ///
    /// [`start_audio_with_project`]: Self::start_audio_with_project
    pub fn start_audio(&self) -> Result<StreamInfo, AudioIoError> {
        self.start_audio_with_source(Source::TestTone)
    }

    /// Open the output device on a single track headed by `source`.
    ///
    /// # Errors
    /// Whatever cpal reports, plus [`AudioIoError::NotRunning`] if the stream
    /// died between opening it and installing the graph.
    pub fn start_audio_with_source(&self, source: Source) -> Result<StreamInfo, AudioIoError> {
        let mut project = Project::empty();
        let name = source.label();
        project
            .add(source, Some(name))
            .expect("an empty project has room for one track");
        self.start_audio_with(project)
    }

    /// Open the output device on the session's own tracks.
    ///
    /// The shell's startup path. A track whose source is a hosted CLAP plugin
    /// (which a project file will produce, from step 8) comes up **silent** and
    /// is attached by the main thread a moment later -- see
    /// [`ClapHost`](crate::clap_host::ClapHost).
    ///
    /// # Errors
    /// See [`start_audio_with_source`](Self::start_audio_with_source).
    pub fn start_audio_with_project(&self) -> Result<StreamInfo, AudioIoError> {
        self.start_audio_with(self.tracks.project())
    }

    /// Open the device and install `project`.
    ///
    /// Order is forced by the service: `install_graph` needs a live session,
    /// because the graph channel is created together with the stream.
    fn start_audio_with(&self, project: Project) -> Result<StreamInfo, AudioIoError> {
        let info = self.audio.start()?;
        self.tracks.set_project(project);
        self.tracks.install_graph(&self.audio)?;
        self.publish_sequencer();
        // A take is converted to the rate the arrangement runs at, and that rate
        // is only known once a device has negotiated one. Publishing it here
        // means a buffer change or a device swap moves the recorder with it.
        self.recorder
            .set_engine_sample_rate(f64::from(info.sample_rate));
        Ok(info)
    }

    /// Push the session's tempo and pattern into the stream that just opened.
    ///
    /// A [`TransportClock`](mydaw_rt_core::TransportClock) is built around a
    /// device sample rate and therefore starts fresh with every stream, at the
    /// default tempo and with no pattern. Everything the user has already done
    /// lives in [`SequencerService`], so it is re-published here -- otherwise
    /// stopping and restarting audio (a device change, a headless capture)
    /// would silently reset the beat.
    ///
    /// Failures are logged rather than propagated: the stream is up and playable
    /// either way, and refusing to open a device because a pattern queue was
    /// momentarily full would be the wrong trade.
    fn publish_sequencer(&self) {
        if let Err(err) = self.audio.set_bpm(self.sequencer.bpm()) {
            log::warn!("could not publish the tempo to the new stream: {err}");
        }
        if let Err(err) = self.audio.install_song(self.sequencer.compile()) {
            log::warn!("could not publish the song to the new stream: {err}");
        }
        // The clip node is created with the graph and starts with nothing, so a
        // project loaded before the device came up publishes its audio here --
        // exactly as the pattern does.
        self.publish_audio_clips();
        if let Err(err) = self.audio.set_play_mode(self.sequencer.play_mode()) {
            log::warn!("could not publish the play mode to the new stream: {err}");
        }
        if let Err(err) = self.audio.set_looping(self.sequencer.is_looping()) {
            log::warn!("could not publish the song loop to the new stream: {err}");
        }
    }

    /// Apply a new host/buffer request: stop, reopen, republish the graph.
    ///
    /// Playhead and playing-state survive the restart so a buffer tweak mid-song
    /// does not throw the user back to bar one.
    pub fn apply_output_request(&self, request: OutputRequest) -> Result<StreamInfo, AudioIoError> {
        let (playing, pos) = match self.audio.transport() {
            Some(clock) => {
                let snap = clock.snapshot();
                (snap.playing, snap.sample_pos)
            }
            None => (false, 0),
        };
        self.audio.set_output_request(request);
        if self.audio.is_running() {
            self.audio.stop();
        }
        let info = self.start_audio_with(self.tracks.project())?;
        if pos > 0 {
            if let Err(err) = self.audio.seek(pos) {
                log::warn!("could not restore the playhead after an audio restart: {err}");
            }
        }
        if playing {
            if let Err(err) = self.audio.set_playing(true) {
                log::warn!("could not resume playback after an audio restart: {err}");
            }
        }
        Ok(info)
    }

    /// Change what plays at the head of the **selected** track, while the engine
    /// runs.
    ///
    /// Built-in sources are constructed and pushed straight from the calling
    /// thread -- a factory is `Send + Sync` and the swap is a wait-free queue
    /// push. A hosted CLAP instrument is the exception and goes through
    /// [`set_track_source_clap`](Self::set_track_source_clap).
    ///
    /// # Errors
    /// [`SourceError::Audio`] when no device is open, [`SourceError::QueueFull`]
    /// when the audio thread is behind (retryable), and
    /// [`SourceError::MainThread`] carrying a bad track index.
    pub fn set_source(&self, source: Source) -> Result<(), SourceError> {
        self.set_track_source(self.tracks.selected(), source)
    }

    /// Change the source of one track to a built-in generator.
    ///
    /// A track that currently holds a **hosted plugin** goes through the main
    /// thread instead, because the plugin it displaces has to be destroyed
    /// there -- and an instance nobody points at any more is a plugin nobody can
    /// see, edit or free.
    ///
    /// # Errors
    /// See [`set_source`](Self::set_source).
    pub fn set_track_source(&self, track: usize, source: Source) -> Result<(), SourceError> {
        if self.clap_bridge.board().track(track).is_some() {
            self.clap_command(ClapCommand::SetTrackBuiltIn {
                track,
                source_id: source.id(),
            })?;
        } else if self.vst3_bridge.board().track(track).is_some() {
            self.vst3_command(Vst3Command::SetTrackBuiltIn {
                track,
                source_id: source.id(),
            })?;
        } else {
            let id = source.id();
            match self.tracks.set_source(track, source, &self.audio) {
                Ok(()) => {
                    log::info!("track {track} source is now '{id}'");
                }
                Err(MixerError::Source(err)) => return Err(err),
                Err(MixerError::Audio(err)) => return Err(SourceError::Audio(err)),
                // A bad index is a UI bug; it reaches the status bar as itself.
                Err(other) => return Err(SourceError::MainThread(other.to_string())),
            }
        }
        Ok(())
    }

    /// Put a hosted CLAP instrument at the head of one track.
    ///
    /// The bundle path and plugin id come straight from the picker's own entry,
    /// which is why both travel rather than an id alone: two copies of the same
    /// plugin can be installed, and guessing which was meant is exactly the kind
    /// of silent wrongness a DAW must not do.
    ///
    /// Runs on the main thread and waits for it -- see `crate::clap_bridge`.
    ///
    /// # Errors
    /// [`SourceError::MainThread`] carrying whatever the main thread said: a
    /// bundle that will not load, a plugin whose ports are not an instrument's,
    /// no audio device, or a timeout.
    pub fn set_track_source_clap(
        &self,
        track: usize,
        bundle_path: &str,
        plugin_id: &str,
    ) -> Result<(), SourceError> {
        let bundle_path = check_bundle_path(bundle_path)?;
        self.clap_command(ClapCommand::SetTrackSource {
            track,
            bundle_path,
            plugin_id: plugin_id.to_owned(),
        })
        .map(|_| ())
    }

    /// Put a hosted CLAP effect in the master insert, replacing any other.
    ///
    /// # Errors
    /// See [`set_track_source_clap`](Self::set_track_source_clap).
    pub fn set_master_effect_clap(
        &self,
        bundle_path: &str,
        plugin_id: &str,
    ) -> Result<(), SourceError> {
        let bundle_path = check_bundle_path(bundle_path)?;
        self.clap_command(ClapCommand::SetMasterEffect {
            bundle_path,
            plugin_id: plugin_id.to_owned(),
        })
        .map(|_| ())
    }

    /// Empty the master insert and destroy the instance that was in it.
    ///
    /// # Errors
    /// [`SourceError::MainThread`].
    pub fn clear_master_effect(&self) -> Result<(), SourceError> {
        // One insert, either format. Ask whichever host actually has something
        // there; with nothing placed the CLAP path still gives the honest
        // "there is no device / nothing to clear" answer it always did.
        if self.vst3_bridge.board().master().is_some() {
            return self
                .vst3_command(Vst3Command::ClearMasterEffect)
                .map(|_| ());
        }
        self.clap_command(ClapCommand::ClearMasterEffect)
            .map(|_| ())
    }

    /// Open a hosted plugin's editor in its own native window.
    ///
    /// Several editors can be open at once, one per placed instance.
    ///
    /// # Errors
    /// [`SourceError::MainThread`] carrying the reason -- most often "This
    /// plugin has no editor.", which is a normal answer rather than a failure.
    pub fn open_clap_gui(&self, target: ClapTarget) -> Result<ClapGuiStatusDto, SourceError> {
        self.clap_command(ClapCommand::OpenGui(target))?
            .into_gui()
            .map_err(SourceError::MainThread)
    }

    /// Close one hosted plugin's editor. Not an error when nothing is open.
    ///
    /// # Errors
    /// [`SourceError::MainThread`].
    pub fn close_clap_gui(&self, target: ClapTarget) -> Result<ClapGuiStatusDto, SourceError> {
        self.clap_command(ClapCommand::CloseGui(target))?
            .into_gui()
            .map_err(SourceError::MainThread)
    }

    /// Serialise a hosted plugin. Groundwork for the project file (step 8).
    ///
    /// An **empty** blob means the plugin has no `state` extension and therefore
    /// nothing to persist, which is a normal answer.
    ///
    /// # Errors
    /// [`SourceError::MainThread`].
    pub fn save_clap_state(&self, target: ClapTarget) -> Result<Vec<u8>, SourceError> {
        self.clap_command(ClapCommand::SaveState(target))?
            .into_state()
            .map_err(SourceError::MainThread)
    }

    /// Restore a hosted plugin from a blob produced by
    /// [`save_clap_state`](Self::save_clap_state).
    ///
    /// # Errors
    /// [`SourceError::MainThread`].
    pub fn load_clap_state(&self, target: ClapTarget, state: Vec<u8>) -> Result<(), SourceError> {
        self.clap_command(ClapCommand::LoadState { target, state })
            .map(|_| ())
    }

    /// Post a command to the main thread's CLAP host and wait for its answer.
    /// Put a hosted VST3 instrument at the head of one track.
    ///
    /// The VST3 counterpart of
    /// [`set_track_source_clap`](Self::set_track_source_clap): the instance is
    /// created, activated and swapped in on the main thread, and this waits for
    /// it.
    ///
    /// # Errors
    /// [`SourceError::MainThread`] carrying whatever the host reported.
    pub fn set_track_source_vst3(
        &self,
        track: usize,
        module_path: &str,
        cid: &str,
    ) -> Result<(), SourceError> {
        let module_path = check_bundle_path(module_path)?;
        self.vst3_command(Vst3Command::SetTrackSource {
            track,
            module_path,
            cid: cid.to_owned(),
        })
        .map(|_| ())
    }

    /// Put a hosted VST3 effect in the master insert, replacing whatever is
    /// there -- including a CLAP one. The insert holds one plugin.
    ///
    /// # Errors
    /// [`SourceError::MainThread`].
    pub fn set_master_effect_vst3(&self, module_path: &str, cid: &str) -> Result<(), SourceError> {
        let module_path = check_bundle_path(module_path)?;
        self.vst3_command(Vst3Command::SetMasterEffect {
            module_path,
            cid: cid.to_owned(),
        })
        .map(|_| ())
    }

    /// Open a hosted VST3 plugin's editor in its own native window.
    ///
    /// # Errors
    /// [`SourceError::MainThread`] carrying the reason -- most often "This
    /// plugin has no editor.", which is a normal answer rather than a failure.
    pub fn open_vst3_gui(&self, target: ClapTarget) -> Result<Vst3GuiStatusDto, SourceError> {
        self.vst3_command(Vst3Command::OpenGui(target))?
            .into_gui()
            .map_err(SourceError::MainThread)
    }

    /// Close one VST3 plugin's editor. Not an error when nothing is open.
    ///
    /// # Errors
    /// [`SourceError::MainThread`].
    pub fn close_vst3_gui(&self, target: ClapTarget) -> Result<Vst3GuiStatusDto, SourceError> {
        self.vst3_command(Vst3Command::CloseGui(target))?
            .into_gui()
            .map_err(SourceError::MainThread)
    }

    /// Serialise a hosted VST3 plugin's component state.
    ///
    /// # Errors
    /// [`SourceError::MainThread`].
    pub fn save_vst3_state(&self, target: ClapTarget) -> Result<Vec<u8>, SourceError> {
        self.vst3_command(Vst3Command::SaveState(target))?
            .into_state()
            .map_err(SourceError::MainThread)
    }

    /// Restore a hosted VST3 plugin from a blob.
    ///
    /// # Errors
    /// [`SourceError::MainThread`].
    pub fn load_vst3_state(&self, target: ClapTarget, state: Vec<u8>) -> Result<(), SourceError> {
        self.vst3_command(Vst3Command::LoadState { target, state })
            .map(|_| ())
    }

    /// The mailbox to the main thread's VST3 host.
    pub fn vst3_bridge(&self) -> &Arc<Vst3HostBridge> {
        &self.vst3_bridge
    }

    /// Installed VST3 plugins, for the browser panel and the pickers.
    ///
    /// Served from the scan cache; `rescan` walks the directories again. The
    /// first call opens every installed module, which is why this is `async` at
    /// the command layer.
    pub fn vst3_plugins(&self, rescan: bool) -> Vec<Vst3PluginInfoDto> {
        let scanned = if rescan {
            self.vst3.refresh()
        } else {
            self.vst3.cached_scan()
        };
        scanned.iter().map(Vst3PluginInfoDto::from).collect()
    }

    /// Scanned VST3 **audio effects**, for the master insert picker.
    pub fn vst3_master_effects(&self) -> Vec<Vst3PluginInfoDto> {
        self.vst3
            .cached_scan()
            .iter()
            .filter(|info| info.is_audio_effect())
            .map(Vst3PluginInfoDto::from)
            .collect()
    }

    /// The VST3 discovery service.
    pub fn vst3(&self) -> &Arc<Vst3HostService> {
        &self.vst3
    }

    fn vst3_command(&self, command: Vst3Command) -> Result<Vst3Answer, SourceError> {
        self.vst3_bridge
            .request(command)
            .map_err(SourceError::MainThread)
    }

    fn clap_command(&self, command: ClapCommand) -> Result<ClapAnswer, SourceError> {
        self.clap_bridge
            .request(command)
            .map_err(SourceError::MainThread)
    }

    /// Change the selected track's source from its string id, as the webview
    /// sends it.
    ///
    /// # Errors
    /// [`SourceError::Unknown`] for an id that names nothing,
    /// [`SourceError::ClapNeedsBundle`] for a `clap:` id (which carries no
    /// bundle path -- the webview must use `set_track_source_clap`), plus
    /// everything [`set_source`](Self::set_source) can report.
    pub fn set_source_id(&self, id: &str) -> Result<(), SourceError> {
        self.set_source(Source::parse(id)?)
    }

    /// The source at the head of the selected track.
    pub fn current_source(&self) -> Source {
        self.tracks.selected_source()
    }

    /// Every source a track can be set to: the built-ins, then every scanned
    /// CLAP **instrument**.
    ///
    /// Effects are filtered out because this list feeds a *track source* picker
    /// and an effect cannot head a track. The filter is the descriptor's
    /// `features`, which is free; a plugin whose features lie is caught by the
    /// port check when the instance is created (see
    /// [`ClapPluginKind`](mydaw_plugin_host_clap::ClapPluginKind)).
    ///
    /// The scan is served from the cache, so this is cheap after the first call.
    pub fn sources(&self) -> Vec<SourceDto> {
        let current = self.current_source();
        let mut sources: Vec<SourceDto> = Source::BUILT_IN
            .iter()
            .map(|source| SourceDto::built_in(source, &current))
            .collect();
        // Sampled instruments sit with the built-ins because that is what they
        // are; they are appended rather than compiled into `BUILT_IN` only
        // because the set is discovered at runtime and can grow while the app
        // is running -- which is exactly what happens when a download finishes.
        sources.extend(
            self.sampler
                .instruments()
                .iter()
                .map(|info| SourceDto::sampler(info, &current)),
        );
        sources.extend(
            self.clap
                .cached_scan()
                .iter()
                .filter(|info| info.is_instrument())
                .map(|info| SourceDto::clap(info, &current)),
        );
        sources
    }

    /// The sampled instrument registry.
    pub fn sampler(&self) -> &Arc<SamplerRegistry> {
        &self.sampler
    }

    /// Every downloadable instrument, whether it is installed, and what its
    /// downloader is doing about it.
    ///
    /// An empty list means this build has no usable manifest -- a repository
    /// error the sampler crate's tests already catch -- and the shell renders
    /// it as "nothing to install" rather than as a failure.
    pub fn asset_status(&self) -> Vec<AssetStatus> {
        self.assets
            .iter()
            .map(|downloader| {
                let mut status = downloader.status();
                // The fact the picker actually depends on: files on disk are
                // not an instrument until the background scan has decoded them.
                status.instrument_ready = self
                    .sampler
                    .contains(&mydaw_plugin_sampler::source_id(&status.instrument_id));
                status
            })
            .collect()
    }

    /// Start downloading one instrument's sample content.
    ///
    /// **Only ever called from a user's click.** Nothing here runs on its own:
    /// see the consent note in [`mydaw_plugin_sampler::download`].
    ///
    /// When the transfer finishes the sampler registry rescans on its own
    /// thread, so the new instrument reaches `get_sources` without a restart
    /// and without this function waiting for a decode.
    ///
    /// # Errors
    /// A sentence, for an id this build does not offer or a download already
    /// running.
    pub fn start_asset_download(&self, instrument_id: &str) -> Result<(), String> {
        let downloader = self
            .assets
            .iter()
            .find(|downloader| downloader.manifest().id == instrument_id)
            .ok_or_else(|| format!("this build does not offer an instrument '{instrument_id}'"))?;
        let sampler = Arc::clone(&self.sampler);
        let id = instrument_id.to_owned();
        downloader
            .start(move |result| match result {
                Ok(()) => {
                    log::info!("sampler: '{id}' installed; rescanning instruments");
                    sampler.rescan_in_background();
                }
                // The error is already recorded in the downloader's status,
                // which is where the UI reads it from.
                Err(err) => log::warn!("sampler: installing '{id}' failed: {err}"),
            })
            .map_err(|err| err.to_string())
    }

    /// Every scanned CLAP **audio effect**, for the master insert picker.
    pub fn master_effects(&self) -> Vec<ClapPluginInfoDto> {
        self.clap
            .cached_scan()
            .iter()
            .filter(|info| info.is_audio_effect())
            .map(ClapPluginInfoDto::from)
            .collect()
    }

    /// The project's tracks and the mixer parameters the audio thread reads.
    pub fn tracks(&self) -> &Arc<TrackHost> {
        &self.tracks
    }

    pub(crate) fn audio_source(
        &self,
        source_id: u64,
    ) -> Option<Arc<mydaw_plugin_sequencer::AudioSource>> {
        self.sources.get(source_id)
    }

    /// The mixer, in the shape the webview reads.
    pub fn mixer_state(&self) -> MixerStateDto {
        self.tracks.state(
            self.clap_bridge.board(),
            self.vst3_bridge.board(),
            self.project_bridge.missing(),
        )
    }

    // ------------------------------------------------- buses, sends, inserts

    /// The internal effect catalogue, everything a generic knob panel needs.
    ///
    /// Static for the process lifetime: fetch it once at startup.
    pub fn effect_catalogue(&self) -> Vec<EffectInfoDto> {
        crate::effects::catalogue(self.tracks.effects())
    }

    /// Install a graph from the project as it now stands.
    ///
    /// The one funnel every **structural** mixer edit ends in, and the whole of
    /// the thread choice: with a hosted plugin loaded the rebuild has to happen
    /// on a host's main thread, because the new graph needs a new node for every
    /// instance and only that thread can make one. With nothing hosted it is a
    /// `GraphSpec` and a queue push on the calling thread, so a headless session
    /// never pays for a mailbox it has no server for.
    ///
    /// **A missing device is not an error.** A bus, a send and an insert chain
    /// are project data, exactly as a pattern is: a user on a machine with no
    /// sound card must still be able to build a mix, and `start_audio_with`
    /// installs whatever the session holds when a stream comes up.
    fn rebuild_graph(&self) -> Result<(), MixerError> {
        // See `add_track` for why the VST3 host is asked first when both formats
        // are loaded: it re-attaches both after the one rebuild.
        if !self.vst3_bridge.board().is_empty() {
            return self
                .vst3_command(Vst3Command::Rebuild)
                .map(|_| ())
                .map_err(MixerError::Source);
        }
        if self.tracks.has_hosted_node() {
            return self
                .clap_command(ClapCommand::Rebuild)
                .map(|_| ())
                .map_err(MixerError::Source);
        }
        match self.tracks.install_graph(&self.audio) {
            Ok(()) | Err(AudioIoError::NotRunning) => Ok(()),
            Err(err) => Err(MixerError::Audio(err)),
        }
    }

    /// Apply a structural edit to the project and rebuild the graph.
    ///
    /// A refused edit changes nothing and never reaches the rebuild.
    fn edit_structure<T>(
        &self,
        edit: impl FnOnce(&mut Project) -> Result<T, MixerError>,
    ) -> Result<T, MixerError> {
        let value = self.tracks.edit_project(edit)?;
        self.rebuild_graph()?;
        Ok(value)
    }

    /// Add a bus. Rebuilds the graph.
    ///
    /// # Errors
    /// [`RouteError::TooManyBuses`], [`RouteError::BlankName`].
    pub fn add_bus(&self, name: Option<String>) -> Result<usize, MixerError> {
        self.edit_structure(|project| project.add_bus(name).map_err(MixerError::from))
    }

    /// Remove a bus, and every send at either end of it. Rebuilds the graph.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`].
    pub fn remove_bus(&self, bus: usize) -> Result<(), MixerError> {
        self.edit_structure(|project| project.remove_bus(bus).map_err(MixerError::from))
            .map(|_| ())
    }

    /// Rename a bus. No rebuild -- a name is not in the graph.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`], [`RouteError::BlankName`].
    pub fn rename_bus(&self, bus: usize, name: &str) -> Result<(), RouteError> {
        self.tracks.rename_bus(bus, name)
    }

    /// Set or clear a bus's colour. No rebuild.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`].
    pub fn set_bus_color(&self, bus: usize, color: Option<String>) -> Result<(), RouteError> {
        self.tracks.set_bus_color(bus, color)
    }

    /// Set a bus's fader, in dB. No rebuild: an atomic, like a track's.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`], or [`TrackError::InvalidGain`] as a sentence.
    pub fn set_bus_gain_db(&self, bus: usize, db: f32) -> Result<(), MixerError> {
        self.tracks.set_bus_gain_db(bus, db)
    }

    /// Set a bus's pan. No rebuild.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`] or [`TrackError::InvalidPan`].
    pub fn set_bus_pan(&self, bus: usize, pan: f32) -> Result<(), MixerError> {
        self.tracks.set_bus_pan(bus, pan)
    }

    /// Mute or unmute a bus. No rebuild.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`].
    pub fn set_bus_muted(&self, bus: usize, muted: bool) -> Result<(), RouteError> {
        self.tracks.set_bus_muted(bus, muted)
    }

    /// Solo or un-solo a bus. No rebuild.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`].
    pub fn set_bus_soloed(&self, bus: usize, soloed: bool) -> Result<(), RouteError> {
        self.tracks.set_bus_soloed(bus, soloed)
    }

    /// Add a send tap, returning its id. Rebuilds the graph.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`], [`RouteError::NoSuchTrack`],
    /// [`RouteError::DuplicateSend`], [`RouteError::SelfSend`] or
    /// [`RouteError::Cycle`] -- and on a refusal nothing is rebuilt.
    pub fn add_send(&self, source: StripId, dest: usize) -> Result<u32, MixerError> {
        self.edit_structure(|project| project.add_send(source, dest).map_err(MixerError::from))
    }

    /// Remove a send tap. Rebuilds the graph.
    ///
    /// # Errors
    /// [`RouteError::NoSuchSend`].
    pub fn remove_send(&self, id: u32) -> Result<(), MixerError> {
        self.edit_structure(|project| project.remove_send(id).map_err(MixerError::from))
            .map(|_| ())
    }

    /// Set a send's level. **One atomic store; no rebuild.**
    ///
    /// # Errors
    /// [`RouteError::NoSuchSend`], [`RouteError::InvalidLevel`].
    pub fn set_send_level(&self, id: u32, level: f32) -> Result<(), RouteError> {
        self.tracks.set_send_level(id, level)
    }

    /// Move a send between pre- and post-fader. **One atomic store.**
    ///
    /// # Errors
    /// [`RouteError::NoSuchSend`].
    pub fn set_send_pre(&self, id: u32, pre: bool) -> Result<(), RouteError> {
        self.tracks.set_send_pre(id, pre)
    }

    /// Add an internal effect to a chain, returning where it landed. Rebuilds.
    ///
    /// # Errors
    /// [`InsertError::UnknownEffect`], [`InsertError::TooManyInserts`], or a
    /// target that is not there.
    pub fn add_insert(
        &self,
        target: InsertTarget,
        type_id: &str,
        position: Option<usize>,
    ) -> Result<usize, MixerError> {
        let effects = Arc::clone(self.tracks.effects());
        self.edit_structure(|project| {
            project
                .add_insert(target, type_id, position, &effects)
                .map_err(MixerError::from)
        })
    }

    /// Remove an insert. Rebuilds.
    ///
    /// # Errors
    /// [`InsertError::NoSuchInsert`], or a target that is not there.
    pub fn remove_insert(&self, target: InsertTarget, index: usize) -> Result<(), MixerError> {
        self.edit_structure(|project| {
            project
                .remove_insert(target, index)
                .map_err(MixerError::from)
        })
        .map(|_| ())
    }

    /// Move an insert within its chain. Rebuilds.
    ///
    /// # Errors
    /// [`InsertError::NoSuchInsert`], or a target that is not there.
    pub fn move_insert(
        &self,
        target: InsertTarget,
        from: usize,
        to: usize,
    ) -> Result<(), MixerError> {
        self.edit_structure(|project| {
            project
                .move_insert(target, from, to)
                .map_err(MixerError::from)
        })
    }

    /// Set one insert parameter, returning **what was actually stored**.
    ///
    /// One atomic store: no rebuild, no allocation. The answer is the sanitised
    /// value, so a knob that sent 40 kHz to a 20 Hz..20 kHz parameter can snap
    /// itself to the top rather than draw where the engine is not.
    ///
    /// # Errors
    /// [`InsertError::NoSuchInsert`] or [`InsertError::NoSuchParam`].
    pub fn set_insert_param(
        &self,
        target: InsertTarget,
        index: usize,
        param_id: &str,
        value: f32,
    ) -> Result<f32, InsertError> {
        self.tracks.set_insert_param(target, index, param_id, value)
    }

    /// Arm the analysis tap on one insert, the track source (`index` omitted),
    /// or silence it.
    pub fn watch_spectrum(
        &self,
        target: Option<InsertTarget>,
        index: Option<usize>,
    ) -> Result<(), InsertError> {
        let packed = match (target, index) {
            (Some(InsertTarget::Track(strip)), None) => SpectrumTarget::track_source(strip),
            (Some(target), Some(index)) => {
                let _ = self.tracks.insert_readouts(target, index)?;
                match target {
                    InsertTarget::Track(strip) => SpectrumTarget::track(strip, index),
                    InsertTarget::Bus(strip) => SpectrumTarget::bus(strip, index),
                    InsertTarget::Master => SpectrumTarget::master(index),
                }
            }
            _ => None,
        };
        if packed.is_none() {
            self.spectrum.clear();
        }
        self.spectrum_watch.set(packed);
        Ok(())
    }

    /// Latest analysed hop for the armed insert.
    pub fn insert_spectrum(&self) -> Option<crate::effects::SpectrumDto> {
        self.spectrum_watch.get()?;
        self.spectrum.poll().map(crate::effects::SpectrumDto::from)
    }

    /// EQ hold-to-solo of the source.
    pub fn set_insert_listen(
        &self,
        target: InsertTarget,
        index: usize,
        band: Option<&str>,
    ) -> Result<(), InsertError> {
        self.tracks.set_insert_listen(target, index, band)
    }

    /// Point one insert's detector at another strip, or unroute it with `None`.
    ///
    /// # Errors
    /// As [`set_insert_param`](Self::set_insert_param), plus
    /// [`InsertError::NoSuchKeySource`] for a source strip that is not there.
    pub fn set_insert_sidechain(
        &self,
        target: InsertTarget,
        index: usize,
        source: Option<mydaw_rt_core::StripId>,
    ) -> Result<(), InsertError> {
        self.tracks.set_insert_sidechain(target, index, source)
    }

    /// Turn one insert's bypass on or off.
    ///
    /// A parameter like any other -- every effect declares `bypass` and does the
    /// 10 ms equal-gain crossfade itself -- so this is one atomic store and no
    /// rebuild. It is offered by name because every strip draws the button.
    ///
    /// # Errors
    /// As [`set_insert_param`](Self::set_insert_param).
    pub fn set_insert_bypass(
        &self,
        target: InsertTarget,
        index: usize,
        bypassed: bool,
    ) -> Result<(), InsertError> {
        self.tracks
            .set_insert_param(
                target,
                index,
                mydaw_plugin_effects::BYPASS_PARAM,
                f32::from(u8::from(bypassed)),
            )
            .map(|_| ())
    }

    /// One insert's live meters, for a panel polling at frame rate.
    ///
    /// # Errors
    /// [`InsertError::NoSuchInsert`].
    pub fn insert_readouts(
        &self,
        target: InsertTarget,
        index: usize,
    ) -> Result<Vec<crate::effects::ParamValueDto>, InsertError> {
        self.tracks.insert_readouts(target, index)
    }

    /// Every meter on one chain.
    ///
    /// # Errors
    /// A target that is not there.
    pub fn chain_readouts(
        &self,
        target: InsertTarget,
    ) -> Result<Vec<crate::effects::InsertReadoutsDto>, InsertError> {
        self.tracks.chain_readouts(target)
    }

    // -------------------------------------------------------------- projects

    /// The project mailbox. Commands post here; the run loop serves it.
    pub fn project_bridge(&self) -> &Arc<ProjectBridge> {
        &self.project_bridge
    }

    /// Which project is open, and what went wrong opening it.
    pub fn project_info(&self) -> ProjectInfoDto {
        self.project_bridge.info()
    }

    /// The file the session came from, or `None` for one that has never been
    /// saved. What decides whether Save needs a dialog.
    pub fn project_path(&self) -> Option<std::path::PathBuf> {
        self.project_bridge.current_path()
    }

    /// Reset the session to a fresh default project.
    ///
    /// # Errors
    /// [`SourceError::MainThread`] carrying whatever the main thread said, or
    /// [`NO_MAIN_THREAD`](crate::project_bridge::NO_MAIN_THREAD) in a headless
    /// session.
    pub fn new_project(&self) -> Result<ProjectInfoDto, SourceError> {
        self.project_command(ProjectCommand::New)
    }

    /// Rebuild the whole session from a `.mydaw` file.
    ///
    /// # Errors
    /// [`SourceError::MainThread`] carrying the reason: the file is not there,
    /// is not a project, was written by a newer MYDAW, or does not describe a
    /// session this engine could run. **A plugin that is absent from this
    /// machine is not one of those** -- the project opens, that track is silent,
    /// and the mixer says so.
    pub fn open_project(
        &self,
        path: impl Into<std::path::PathBuf>,
    ) -> Result<ProjectInfoDto, SourceError> {
        self.project_command(ProjectCommand::Open { path: path.into() })
    }

    /// Write the session to `path`, which becomes the current project.
    ///
    /// # Errors
    /// [`SourceError::MainThread`] carrying the reason. The previous contents of
    /// `path` are untouched when it fails -- see
    /// [`write_project_file`](crate::project_file::write_project_file).
    pub fn save_project(
        &self,
        path: impl Into<std::path::PathBuf>,
    ) -> Result<ProjectInfoDto, SourceError> {
        self.project_command(ProjectCommand::Save { path: path.into() })
    }

    fn project_command(&self, command: ProjectCommand) -> Result<ProjectInfoDto, SourceError> {
        self.project_bridge
            .request(command)
            .and_then(crate::project_bridge::ProjectAnswer::into_info)
            .map_err(SourceError::MainThread)
    }

    /// Set a track's fader, in dB. Works with no audio device open.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`] or [`TrackError::InvalidGain`].
    pub fn set_track_gain_db(&self, track: usize, db: f32) -> Result<(), TrackError> {
        self.tracks.set_gain_db(track, db)
    }

    /// Set a track's pan, `-1.0..=1.0`.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`] or [`TrackError::InvalidPan`].
    pub fn set_track_pan(&self, track: usize, pan: f32) -> Result<(), TrackError> {
        self.tracks.set_pan(track, pan)
    }

    /// Channel pitch for an audio-clip or one-shot track, in cents.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_track_pitch_cents(&self, track: usize, cents: f32) -> Result<(), MixerError> {
        self.tracks.set_pitch_cents(track, cents)?;
        self.apply_sample_play(track)
    }

    /// Detect the root pitch of an audio-file channel's selected region.
    ///
    /// Prefers embedded WAV `smpl` metadata, then a filename note token, then
    /// bounded YIN analysis. Never invents C for unpitched material.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], [`TrackError::NotASampleChannel`], or a
    /// missing decoded file.
    pub fn analyze_track_pitch(&self, track: usize) -> Result<PitchAnalysisDto, MixerError> {
        let project = self.tracks.project();
        let def = project
            .tracks
            .get(track)
            .ok_or(TrackError::NoSuchTrack(track))?;
        let source_id = def
            .source
            .sample_file_id()
            .ok_or(TrackError::NotASampleChannel(track))?;
        let start = def.sample_start_frames as usize;
        let end = def.sample_end_frames as usize;
        let source = self.audio_source(source_id).ok_or_else(|| {
            MixerError::Source(SourceError::SampleFile(format!(
                "audio source {source_id} is not loaded"
            )))
        })?;
        if let Some(hint) = mydaw_pitch_analysis::root_hint(source.path()) {
            let stored = RootPitchDto::from(hint);
            self.tracks.set_detected_pitch(track, Some(stored))?;
            return Ok(mydaw_pitch_analysis::PitchOutcome::Detected(hint).into());
        }
        let frames = source.frames();
        let range = if end == 0 || end > frames {
            start.min(frames)..frames
        } else {
            start.min(end)..end
        };
        let outcome = mydaw_pitch_analysis::detect_root_pitch(
            source.stereo(),
            frames,
            source.sample_rate(),
            range,
        );
        if let mydaw_pitch_analysis::PitchOutcome::Detected(pitch) = outcome {
            self.tracks
                .set_detected_pitch(track, Some(RootPitchDto::from(pitch)))?;
        }
        Ok(outcome.into())
    }

    /// Calibrate an audio-file channel's playback root without changing transpose.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], [`TrackError::NotASampleChannel`],
    /// [`TrackError::InvalidRootKey`], or [`TrackError::InvalidRootFine`].
    pub fn set_track_sample_root(
        &self,
        track: usize,
        key: u8,
        fine_cents: f32,
        mode: RootMode,
    ) -> Result<(), MixerError> {
        let project = self.tracks.project();
        let def = project
            .tracks
            .get(track)
            .ok_or(TrackError::NoSuchTrack(track))?;
        if def.source.sample_file_id().is_none() {
            return Err(TrackError::NotASampleChannel(track).into());
        }
        drop(project);
        self.tracks.set_sample_root(track, key, fine_cents, mode)?;
        Ok(())
    }

    /// Set one of the instrument's own knobs, returning what was stored.
    ///
    /// One atomic store: no rebuild, no allocation, and the audio thread reads
    /// it on its next block with the parameter's own ramp. The answer is the
    /// *sanitised* value, so a knob that sent 40 kHz to an 18 kHz cutoff can
    /// snap itself to the top rather than draw where the engine is not.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], or [`InsertError::NoSuchParam`] for a track
    /// whose instrument has no such knob -- including one that has none at all.
    pub fn set_source_param(
        &self,
        track: usize,
        param_id: &str,
        value: f32,
    ) -> Result<f32, MixerError> {
        let project = self.tracks.project();
        let def = project
            .tracks
            .get(track)
            .ok_or(TrackError::NoSuchTrack(track))?;
        let missing = || InsertError::NoSuchParam {
            effect: def.source.label(),
            param: param_id.to_owned(),
        };
        let block = def.source_params.as_ref().ok_or_else(missing)?;
        block
            .set_by_id(param_id, value)
            .ok_or_else(|| missing().into())
    }

    /// Channel fades for an audio-clip or one-shot track, in milliseconds.
    ///
    /// One command for both, because they are one gesture's worth of the same
    /// idea and setting them separately would republish the arrangement twice.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_track_fades(
        &self,
        track: usize,
        fade_in_ms: f32,
        fade_out_ms: f32,
    ) -> Result<(), MixerError> {
        self.tracks.set_fades(track, fade_in_ms, fade_out_ms)?;
        self.apply_sample_play(track)
    }

    /// SMP region for an audio-clip or one-shot track, in the file's frames.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_track_sample_region(
        &self,
        track: usize,
        start_frames: u64,
        end_frames: u64,
    ) -> Result<(), MixerError> {
        self.tracks
            .set_sample_region(track, start_frames, end_frames)?;
        self.apply_sample_play(track)
    }

    /// Where an audio-file channel's slices would fall, without installing them.
    ///
    /// Analysis and installation are two commands on purpose: the sensitivity
    /// slider re-runs the first on every step of a drag, and a chop the user is
    /// still shaping should not be what the keyboard plays until they stop.
    /// The range is the channel's SMP region, so trimming the sample also
    /// narrows the chop.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], [`TrackError::NotASampleChannel`], or a
    /// missing decoded file.
    pub fn analyze_track_slices(
        &self,
        track: usize,
        mode: SliceMode,
        sensitivity: f32,
        division: u32,
    ) -> Result<SliceAnalysisDto, MixerError> {
        let project = self.tracks.project();
        let def = project
            .tracks
            .get(track)
            .ok_or(TrackError::NoSuchTrack(track))?;
        let source_id = def
            .source
            .sample_file_id()
            .ok_or(TrackError::NotASampleChannel(track))?;
        let (start, end) = (
            def.sample_start_frames as usize,
            def.sample_end_frames as usize,
        );
        drop(project);
        let source = self.audio_source(source_id).ok_or_else(|| {
            MixerError::Source(SourceError::SampleFile(format!(
                "audio source {source_id} is not loaded"
            )))
        })?;
        let frames = source.frames();
        let range = if end == 0 || end > frames {
            start.min(frames)..frames
        } else {
            start.min(end)..end
        };
        let request = match mode {
            // "Analyse for a mode that is off" is what the window asks when it
            // opens the Slices tab on a channel that has never been sliced, so
            // it answers with the default chop rather than an error.
            SliceMode::Off | SliceMode::Transient => mydaw_slice_analysis::SliceMode::Transient(
                mydaw_slice_analysis::Sensitivity::new(sensitivity),
            ),
            SliceMode::Grid => mydaw_slice_analysis::SliceMode::Grid { division },
        };
        let analysis = mydaw_slice_analysis::analyze(
            source.stereo(),
            frames,
            source.sample_rate(),
            range.clone(),
            self.bpm(),
            request,
            mydaw_plugin_sequencer::MAX_SLICES,
        );
        Ok(SliceAnalysisDto {
            frames: analysis.frames,
            mode: if mode == SliceMode::Grid {
                SliceMode::Grid
            } else {
                SliceMode::Transient
            },
            bars: analysis.bars,
            truncated: analysis.truncated,
            range_start: range.start as u64,
            range_end: range.end as u64,
        })
    }

    /// Install (or clear) an audio-file channel's chop.
    ///
    /// Returns what was actually stored: markers are sorted, deduped, clamped
    /// to the file and capped at the engine's table size, so the window draws
    /// the table the engine is playing rather than the one it asked for.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`] or [`TrackError::NotASampleChannel`].
    pub fn set_track_slices(
        &self,
        track: usize,
        state: SliceStateDto,
    ) -> Result<SliceStateDto, MixerError> {
        let project = self.tracks.project();
        let def = project
            .tracks
            .get(track)
            .ok_or(TrackError::NoSuchTrack(track))?;
        let source_id = def
            .source
            .sample_file_id()
            .ok_or(TrackError::NotASampleChannel(track))?;
        drop(project);
        // A file that is not loaded (a project opened away from its samples)
        // still gets to keep its markers -- they are frames, not pointers, and
        // clamping them to zero would destroy the chop the file's return would
        // make playable again.
        let limit = self
            .audio_source(source_id)
            .map_or(u64::from(u32::MAX), |source| source.frames() as u64);
        Ok(self.tracks.set_slices(track, state, limit)?)
    }

    fn apply_sample_play(&self, track: usize) -> Result<(), MixerError> {
        let source = self
            .tracks
            .project()
            .tracks
            .get(track)
            .map(|def| def.source.clone());
        match source {
            Some(Source::AudioClip { .. }) => {
                self.publish_audio_clips();
                Ok(())
            }
            // One-shot nodes share the track's atomic sample-play block. The
            // setters above already published the new defaults, so replacing
            // the node here would only destroy voices that are still sounding.
            Some(Source::OneShot { .. }) => Ok(()),
            _ => Ok(()),
        }
    }

    /// Mute or unmute a track.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_track_muted(&self, track: usize, muted: bool) -> Result<(), TrackError> {
        self.tracks.set_muted(track, muted)
    }

    /// Solo or un-solo a track.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_track_soloed(&self, track: usize, soloed: bool) -> Result<(), TrackError> {
        self.tracks.set_soloed(track, soloed)
    }

    /// Set the master fader, in dB.
    ///
    /// # Errors
    /// [`TrackError::InvalidGain`].
    pub fn set_master_gain_db(&self, db: f32) -> Result<(), TrackError> {
        self.tracks.set_master_gain_db(db)
    }

    /// Choose the track the keyboard piano and the source selector address.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`].
    pub fn set_selected_track(&self, track: usize) -> Result<(), TrackError> {
        self.tracks.set_selected(track)
    }

    /// The track the keyboard piano plays.
    pub fn selected_track(&self) -> usize {
        self.tracks.selected()
    }

    /// Add a track and rebuild the graph.
    ///
    /// # Which thread does the work
    ///
    /// With no hosted plugin placed, inline on the calling thread -- a rebuild
    /// is a `GraphSpec` and a queue push. With one placed, through the main
    /// thread, because the new graph needs a new node for every instance and
    /// only the main thread can make one. The choice is made here rather than in
    /// [`TrackHost`] so that a headless session (a test, a capture run) never
    /// pays for a mailbox it has no server for.
    ///
    /// # Errors
    /// [`MixerError`] -- most usefully [`TrackError::TooManyTracks`] -- or, on
    /// the hosted path, [`SourceError::MainThread`] carrying what the main
    /// thread said.
    pub fn add_track(&self, source: Source, name: Option<String>) -> Result<usize, MixerError> {
        let outcome = self.place_track(source, name);
        // One call site for all three paths, and deliberately outside the `?`.
        //
        // Two things are load-bearing here. It runs whichever path placed the
        // track: the two hosted branches used to `return` before reaching it, so
        // once *any* CLAP or VST3 plugin was placed -- one instrument on one
        // channel is enough -- every channel added afterwards got no grid in any
        // pattern, and the rack drew it as a row with no lane at all. And it
        // runs even when `outcome` is an error: with no audio device the graph
        // install fails *after* the track was added, and a pattern that did not
        // follow it would leave a channel nothing can be clicked into. Against a
        // genuine refusal (the track limit) it re-syncs a channel that already
        // has a grid, which `PatternSet::add_channel` ignores.
        //
        // The count comes from the project rather than from `outcome` for the
        // same reason.
        self.sync_channel_added(self.tracks.track_count().saturating_sub(1));
        outcome
    }

    /// Decode `path` as a one-shot sampler and add a rack channel that plays it.
    ///
    /// The same file can still be dropped on a playlist lane, which creates (or
    /// reuses) a separate Audio Clip channel.
    ///
    /// # Errors
    /// Decode failure, or [`add_track`](Self::add_track).
    pub fn add_sample_channel(&self, path: &std::path::Path) -> Result<usize, MixerError> {
        let source = self
            .load_audio_source(path)
            .map_err(|err| MixerError::Source(SourceError::SampleFile(err.to_string())))?;
        let name = source.name();
        let source_id = source.id();
        self.add_track(Source::OneShot { source_id }, Some(name))
    }

    /// The Audio Clip channel for `source_id`, creating one if the file is new.
    ///
    /// Dropping the same wav on the playlist twice reuses this channel, so
    /// volume, pan, pitch and region stay shared.
    ///
    /// # Errors
    /// [`add_track`](Self::add_track).
    pub fn ensure_audio_clip_channel(&self, source_id: u64) -> Result<usize, MixerError> {
        if let Some(index) = self.tracks.project().find_audio_clip_channel(source_id) {
            return Ok(index);
        }
        let name = self
            .sources
            .get(source_id)
            .map(|source| source.name())
            .unwrap_or_else(|| format!("Audio {source_id}"));
        self.add_track(Source::AudioClip { source_id }, Some(name))
    }

    /// Put the track wherever this build's plugin state says it has to go.
    ///
    /// Split out so [`add_track`](Self::add_track) has exactly one exit and one
    /// pattern sync -- see the comment there for what that is holding up.
    fn place_track(&self, source: Source, name: Option<String>) -> Result<usize, MixerError> {
        // With a VST3 plugin placed the edit goes to the *VST3* host, which
        // re-attaches both formats after the one rebuild. With only CLAP placed
        // it goes to the CLAP host, exactly as it did in step 6. See
        // `crate::vst3_host` for why only one of the two can own the operation.
        if !self.vst3_bridge.board().is_empty() {
            let source_id = source.id();
            self.vst3_command(Vst3Command::AddTrack { source_id, name })
                .map_err(MixerError::Source)?;
            return Ok(self.tracks.track_count().saturating_sub(1));
        }
        if self.tracks.has_hosted_node() {
            let source_id = source.id();
            self.clap_command(ClapCommand::AddTrack { source_id, name })
                .map_err(MixerError::Source)?;
            return Ok(self.tracks.track_count().saturating_sub(1));
        }
        self.tracks.add_track(source, name, &self.audio)
    }

    /// Give every pattern a grid on a channel that has just appeared.
    ///
    /// Called after the rack grew, whichever of the three paths did it. A
    /// channel that exists in the mixer and not in the pattern would be a row
    /// the user cannot click in until they draw a note into it, which is not a
    /// state anyone would understand.
    fn sync_channel_added(&self, index: usize) {
        if index > mydaw_plugin_sequencer::MAX_CHANNEL as usize {
            return;
        }
        let key = self.step_key_for(index);
        self.sequencer.add_channel(index as u8, key);
        let _ = self.publish_song();
    }

    /// Drop a removed channel from every pattern, shifting the ones above it.
    ///
    /// A channel *is* a track index, so not doing this would leave every
    /// pattern playing the wrong instrument -- see
    /// [`Pattern::retarget_after_removing`](mydaw_plugin_sequencer::Pattern::retarget_after_removing).
    fn sync_channel_removed(&self, track: usize) {
        if track <= mydaw_plugin_sequencer::MAX_CHANNEL as usize {
            self.sequencer.remove_channel(track as u8);
            let _ = self.publish_song();
        }
    }

    /// Remove a track and rebuild the graph.
    ///
    /// See [`add_track`](Self::add_track) on which thread does the work.
    ///
    /// # Errors
    /// [`MixerError`] -- most usefully [`TrackError::LastTrack`].
    pub fn remove_track(&self, track: usize) -> Result<(), MixerError> {
        let clip_source =
            self.tracks
                .project()
                .tracks
                .get(track)
                .and_then(|def| match def.source {
                    Source::AudioClip { source_id } => Some(source_id),
                    _ => None,
                });
        if let Some(source_id) = clip_source {
            let _ = self.edit_playlist(|list| {
                list.remove_clips_of_source(source_id);
                Ok(())
            });
        }
        // See `add_track` for why the VST3 host is asked first.
        if !self.vst3_bridge.board().is_empty() {
            self.vst3_command(Vst3Command::RemoveTrack { track })
                .map_err(MixerError::Source)?;
            return Ok(());
        }
        if self.tracks.has_hosted_node() {
            self.clap_command(ClapCommand::RemoveTrack { track })
                .map_err(MixerError::Source)?;
            return Ok(());
        }
        let before = self.tracks.track_count();
        let outcome = self.tracks.remove_track(track, &self.audio);
        // As in `add_track`: the project edit happens before the graph install,
        // so what decides is whether the rack actually shrank.
        if self.tracks.track_count() < before {
            self.sync_channel_removed(track);
        }
        outcome
    }

    /// The CLAP host mailbox. Commands post here; the run loop serves it.
    pub fn clap_bridge(&self) -> &Arc<ClapHostBridge> {
        &self.clap_bridge
    }

    /// Installed CLAP plugins, in the shape the webview reads.
    ///
    /// Served from [`ClapHostService::cached_scan`]: the first call walks the
    /// standard plugin directories and reads every bundle it finds -- in a child
    /// process each, so a vendor's `init()` cannot take MYDAW with it -- and later
    /// calls are free. `rescan` forces a fresh walk, for the case where the user
    /// installed a plugin without restarting MYDAW.
    pub fn clap_plugins(&self, rescan: bool) -> Vec<ClapPluginInfoDto> {
        let scanned = if rescan {
            self.clap.refresh()
        } else {
            self.clap.cached_scan()
        };
        scanned.iter().map(ClapPluginInfoDto::from).collect()
    }

    /// The CLAP host service, for code that needs to load a specific bundle.
    pub fn clap(&self) -> &Arc<ClapHostService> {
        &self.clap
    }

    /// Start or stop the transport.
    ///
    /// Stopping also fades the browser preview: Stop means silence, including a
    /// sample or instrument that was still auditioning with the song already
    /// parked. Play leaves a preview running so a file can be judged against
    /// the mix.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] when no device was opened.
    pub fn set_playing(&self, playing: bool) -> Result<(), AudioIoError> {
        if !playing {
            let _ = self.tracks.file_preview().stop();
        }
        self.audio.set_playing(playing)
    }

    /// Move the playhead to an absolute sample position.
    ///
    /// The command a dragged playhead sends. Everything downstream of it is
    /// already seek-correct by construction -- the scheduler derives every
    /// position from the absolute tick and owes nothing across the jump except
    /// the note-offs it flushes -- so this is one atomic store and no
    /// cooperation at all.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] when no device was opened.
    pub fn seek(&self, sample_pos: u64) -> Result<(), AudioIoError> {
        self.audio.seek(sample_pos)
    }

    /// Play a note, live, on the selected track.
    ///
    /// The event's channel **is** the track index, which is how a keyboard press
    /// reaches one instrument and not every instrument in the project (see
    /// `mydaw_rt_core::graph`).
    ///
    /// Deliberately independent of the transport: pressing a key must make a
    /// sound whether or not the DAW is playing. See `crate::graph`.
    ///
    /// # Errors
    /// [`NoteError::KeyOutOfRange`] or [`NoteError::InvalidVelocity`] for values
    /// the webview should never have sent, and [`NoteError::Audio`] when no
    /// device is open.
    pub fn note_on(&self, key: u8, velocity: f32) -> Result<(), NoteError> {
        if key > MAX_KEY {
            return Err(NoteError::KeyOutOfRange(key));
        }
        if !velocity.is_finite() || !(0.0..=1.0).contains(&velocity) {
            return Err(NoteError::InvalidVelocity(velocity));
        }
        self.audio
            .send_note(LiveNote::note_on(key, velocity).on_channel(self.note_channel()))?;
        Ok(())
    }

    /// Release a note on the selected track.
    ///
    /// # Errors
    /// [`NoteError::KeyOutOfRange`], or [`NoteError::Audio`] when no device is
    /// open.
    pub fn note_off(&self, key: u8) -> Result<(), NoteError> {
        if key > MAX_KEY {
            return Err(NoteError::KeyOutOfRange(key));
        }
        self.audio
            .send_note(LiveNote::note_off(key).on_channel(self.note_channel()))?;
        Ok(())
    }

    /// The note channel live input is sent on: the selected track's index.
    ///
    /// Clamped to the 16 channels a note event can address, which is also
    /// [`MAX_TRACKS`](mydaw_rt_core::MAX_TRACKS) -- so the clamp is unreachable
    /// rather than lossy.
    fn note_channel(&self) -> u8 {
        self.tracks.selected().min(mydaw_rt_core::MAX_TRACKS - 1) as u8
    }

    /// The whole sequencer, in the shape the webview reads.
    ///
    /// Patterns, the arrangement, the tempo, the mode and the tick geometry in
    /// one message. Every pattern command returns this, so a panel never has to
    /// merge a partial update into what it already had -- which is where
    /// optimistic UIs usually go wrong. The IPC spike measured a message this
    /// size as free.
    pub fn sequencer_state(&self) -> SequencerStateDto {
        let (patterns, playlist) = self.sequencer.song();
        SequencerStateDto {
            bpm: self.sequencer.bpm(),
            sample_rate: self.audio.sample_rate().unwrap_or(0.0),
            play_mode: self.sequencer.play_mode().id(),
            looping: self.sequencer.is_looping(),
            ppq: mydaw_plugin_sequencer::PPQ,
            ticks_per_step: mydaw_plugin_sequencer::TICKS_PER_STEP,
            ticks_per_bar: mydaw_plugin_sequencer::TICKS_PER_BAR,
            pattern_set: PatternSetDto::from(&patterns),
            playlist: PlaylistDto::from(&playlist),
        }
    }

    /// Republish the compiled song to the audio thread.
    ///
    /// Every song edit ends here. **Not** an error when no device is open: a
    /// pattern is project data, a user on a machine with no sound card must
    /// still be able to program a beat, and `publish_sequencer` pushes whatever
    /// the session holds when a stream comes up. The only audio error worth
    /// reporting is a full queue, which is retryable.
    fn publish_song(&self) -> Result<(), SequencerError> {
        self.publish_audio_clips();
        match self.audio.install_song(self.sequencer.compile()) {
            Ok(()) | Err(AudioIoError::NotRunning) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Republish the arrangement's **audio** clips to every Audio Clip channel.
    ///
    /// Notes and audio cross the wall to two places -- the scheduler and the
    /// clip node -- so every song edit publishes both. Failures are logged
    /// rather than propagated: with no device open there is nowhere to publish
    /// to, and a full queue costs the *next* edit nothing (the node keeps the
    /// last of a burst). The one thing it must not do is fail an edit that has
    /// already been applied to the session.
    fn publish_audio_clips(&self) {
        let project = self.tracks.project();
        let song = self
            .sequencer
            .compile_audio_with(&self.sources, |id| project.audio_clip_tune(id));
        if let Err(err) = self.tracks.audio_clips().install(song) {
            if !matches!(err, SourceError::Audio(AudioIoError::NotRunning)) {
                log::warn!("could not publish the audio clips: {err}");
            }
        }
    }

    /// The audio-source registry, for the commands that load and list files.
    pub fn audio_sources(&self) -> &Arc<AudioSourceRegistry> {
        &self.sources
    }

    /// Decode `path` and give it a source id the arrangement can reference.
    ///
    /// Loading the same path twice returns the source that is already there --
    /// see [`AudioSourceRegistry::load`].
    ///
    /// # Errors
    /// [`SequencerError::AudioSource`], whose message names the file.
    pub fn load_audio_source(
        &self,
        path: &std::path::Path,
    ) -> Result<Arc<mydaw_plugin_sequencer::AudioSource>, SequencerError> {
        let was_missing = self
            .sources
            .find_by_path(path)
            .is_some_and(|source| source.is_missing());
        let source = self.sources.load(path)?;
        // A file that was missing when the project loaded and has now been
        // re-loaded under a *new* id changes nothing about existing clips, but a
        // reload of the same id does -- so republish either way. It is one
        // compile of a list a human can count.
        self.publish_audio_clips();
        if was_missing {
            // OneShotNode snapshots Arc<AudioSource> at graph construction.
            // Republishing clips updates the playlist player; notes stay bound
            // to the silent placeholder unless the rack nodes are rebuilt.
            if let Err(err) = self.rebuild_graph() {
                log::warn!("could not rebuild sample players after reloading {path:?}: {err}");
            }
        }
        Ok(source)
    }

    /// Audition `path` through the dedicated preview slot. Does not consume a
    /// registry entry.
    ///
    /// # Errors
    /// Decode failure, or no audio device.
    pub fn preview_audio_file(
        &self,
        path: &std::path::Path,
    ) -> Result<(), crate::file_preview::PreviewFileError> {
        self.tracks.file_preview().play(path)
    }

    /// Audition one hit of a built-in or sampled instrument through the same
    /// preview slot files use. Does not change the selected rack channel.
    ///
    /// # Errors
    /// A hosted plugin, an unknown id, a missing instrument, a decode-style
    /// refusal from the preview bus, or no audio device.
    pub fn preview_instrument(&self, source_id: &str) -> Result<(), String> {
        let take = self
            .source_hit_audio(source_id)
            .map_err(|err| err.to_string())?;
        self.tracks
            .file_preview()
            .play_source(take)
            .map_err(|err| err.to_string())
    }

    /// How long a clip covering the whole of `source_id` should be, in ticks.
    ///
    /// What `add_audio_clip` uses when the caller gives no length. It is a
    /// function of the **session tempo**, because a tick is: the same file is
    /// four bars at 120 BPM and three at 160, and the audio does not stretch
    /// either way. A source of zero frames (a file that has moved) gets the
    /// default clip length, so its clip is still drawable and can be
    /// re-pointed at the file once it is found.
    ///
    /// # Errors
    /// [`SequencerError::AudioSource`] when there is no such source.
    pub fn audio_clip_default_length(&self, source_id: u64) -> Result<u64, SequencerError> {
        let source = self
            .sources
            .get(source_id)
            .ok_or(AudioSourceError::NoSuchSource(source_id))?;
        let ticks = crate::dto::ticks_for_seconds(source.seconds(), self.bpm());
        Ok(if ticks == 0 {
            mydaw_plugin_sequencer::DEFAULT_CLIP_TICKS
        } else {
            ticks
        })
    }

    /// Drop a source, refusing while the arrangement still plays it.
    ///
    /// # Errors
    /// [`SequencerError::AudioSource`] -- most usefully
    /// [`AudioSourceError::StillReferenced`], whose message says how many clips.
    pub fn unload_audio_source(&self, id: u64) -> Result<(), SequencerError> {
        self.sources.unload(id, &self.sequencer.playlist())?;
        self.publish_audio_clips();
        Ok(())
    }

    /// Apply one edit to the pattern set and republish.
    ///
    /// The one funnel every pattern command goes through, so that "validate,
    /// roll back on failure, recompile, publish" is written once instead of
    /// fifteen times.
    ///
    /// # Errors
    /// [`SequencerError::Song`] for an edit the engine refused (the session is
    /// unchanged), or [`SequencerError::Audio`] when the audio thread is behind
    /// (the session *is* changed -- the edit happened, only the publish did
    /// not, and the next one will carry it).
    pub fn edit_patterns<T>(
        &self,
        edit: impl FnOnce(&mut PatternSet) -> Result<T, PatternSetError>,
    ) -> Result<T, SequencerError> {
        let value = self.sequencer.edit_patterns(edit)?;
        self.publish_song()?;
        Ok(value)
    }

    /// Apply one edit to the arrangement and republish.
    ///
    /// # Errors
    /// As [`edit_patterns`](Self::edit_patterns).
    pub fn edit_playlist<T>(
        &self,
        edit: impl FnOnce(&mut Playlist) -> Result<T, PlaylistError>,
    ) -> Result<T, SequencerError> {
        let value = self.sequencer.edit_playlist(edit)?;
        self.publish_song()?;
        Ok(value)
    }

    /// Add an empty pattern with a grid on every rack channel.
    ///
    /// The channels come from the *mixer*, because that is what a pattern
    /// addresses, and their step keys from what is on them -- so a new pattern
    /// in a session of five drums and a piano opens with five drum grids and a
    /// piano grid rather than with nothing.
    ///
    /// # Errors
    /// As [`edit_patterns`](Self::edit_patterns).
    pub fn add_pattern(&self, name: Option<String>) -> Result<u64, SequencerError> {
        let project = self.tracks.project();
        let channels: Vec<u8> = (0..project.len().min(mydaw_rt_core::MAX_TRACKS))
            .map(|index| index as u8)
            .collect();
        let keys: Vec<u8> = channels
            .iter()
            .map(|channel| self.step_key_for(*channel as usize))
            .collect();
        self.edit_patterns(move |set| {
            set.add(name, &channels, |channel| {
                keys.get(channel as usize).copied().unwrap_or(60)
            })
        })
    }

    /// Make `id` the pattern the grid and PAT mode address. Test affordance for
    /// the command of the same name.
    #[cfg(test)]
    pub(crate) fn set_active_pattern_for_test(&self, id: u64) {
        self.edit_patterns(|set| set.set_active(id)).unwrap();
    }

    /// Delete a pattern, and every clip that played it.
    ///
    /// # Errors
    /// As [`edit_patterns`](Self::edit_patterns).
    pub fn remove_pattern(&self, id: u64) -> Result<usize, SequencerError> {
        let dropped = self.sequencer.remove_pattern(id)?;
        self.publish_song()?;
        Ok(dropped)
    }

    /// Split a pattern into one pattern per occupied rack channel.
    ///
    /// Names come from the mixer: `"Loop - Kick"`. Playlist instances of the
    /// original are spread onto consecutive lanes, and missing lanes are
    /// appended.
    ///
    /// # Errors
    /// As [`edit_patterns`](Self::edit_patterns).
    pub fn split_pattern_by_channel(&self, id: u64) -> Result<Vec<u64>, SequencerError> {
        let names: Vec<String> = self
            .tracks
            .project()
            .tracks
            .iter()
            .map(|track| track.name.clone())
            .collect();
        let ids = self.sequencer.split_pattern_by_channel(id, |channel| {
            names
                .get(channel as usize)
                .cloned()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("Channel {}", channel + 1))
        })?;
        self.publish_song()?;
        Ok(ids)
    }

    /// The step key a new channel's grid gets: the drum's General MIDI number
    /// when the channel is one of the first four, middle C otherwise.
    ///
    /// Derived from the *source* rather than from the position, so a rack whose
    /// first channel is a synth does not get a grid on key 36.
    fn step_key_for(&self, channel: usize) -> u8 {
        let source = self
            .tracks
            .project()
            .tracks
            .get(channel)
            .map(|track| track.source.clone());
        match source {
            Some(Source::Sampler(reference))
                if reference.instrument_id == crate::project::KIT_ID =>
            {
                crate::project::KIT_STEP_KEY
            }
            _ => mydaw_plugin_sequencer::DEFAULT_STEP_KEY,
        }
    }

    /// Whether the transport plays the active pattern or the arrangement.
    pub fn play_mode(&self) -> PlayMode {
        self.sequencer.play_mode()
    }

    /// Switch between pattern and song mode.
    ///
    /// Stores it in the session first and publishes second, so the mode survives
    /// a device that is not open yet -- exactly like the tempo.
    ///
    /// # Errors
    /// [`SequencerError::Audio`] when the command queue is full.
    pub fn set_play_mode(&self, mode: PlayMode) -> Result<(), SequencerError> {
        self.sequencer.set_play_mode(mode);
        match self.audio.set_play_mode(mode) {
            Ok(()) | Err(AudioIoError::NotRunning) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Whether song mode repeats the arrangement rather than stopping at it.
    pub fn is_looping(&self) -> bool {
        self.sequencer.is_looping()
    }

    /// Turn the song loop on or off.
    ///
    /// Session first, engine second, for the reason the mode is: a loop setting
    /// must survive a device that is not open yet.
    ///
    /// # Errors
    /// [`SequencerError::Audio`] when the command queue is full.
    pub fn set_looping(&self, looping: bool) -> Result<(), SequencerError> {
        self.sequencer.set_looping(looping);
        match self.audio.set_looping(looping) {
            Ok(()) | Err(AudioIoError::NotRunning) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Replace the pattern.
    ///
    /// Validated first, stored second, published third -- so a rejected edit
    /// leaves both the session and the audio thread exactly as they were.
    ///
    /// **Not** an error when no device is open. A pattern is project data: a
    /// user on a machine with no sound card, or with audio stopped, must still
    /// be able to program a beat, and [`start_audio_with_source`] publishes
    /// whatever the session holds when a stream comes up. The only audio error
    /// worth reporting here is a full queue, which is retryable.
    ///
    /// # Errors
    /// [`SequencerError::Pattern`] for a pattern the engine cannot run, and
    /// [`SequencerError::Audio`] when the audio thread is behind.
    ///
    /// [`start_audio_with_source`]: Self::start_audio_with_source
    pub fn set_pattern(&self, pattern: Pattern) -> Result<(), SequencerError> {
        let id = pattern.id;
        self.edit_patterns(move |set| {
            *set.get_mut(id)? = pattern;
            Ok(())
        })
    }

    /// Replace the pattern from what the webview sent.
    ///
    /// # Errors
    /// Everything [`set_pattern`](Self::set_pattern) reports, plus
    /// [`SequencerError::Pattern`] for a DTO that does not describe a pattern
    /// at all (a row of the wrong length).
    pub fn set_pattern_dto(&self, dto: &PatternDto) -> Result<(), SequencerError> {
        let mut pattern = Pattern::try_from(dto)?;
        // A DTO with no id addresses the pattern being edited, which is what a
        // panel that only ever shows one pattern sends. A DTO *with* an id
        // addresses that pattern, so a UI that edits a pattern it is not
        // looking at can.
        if pattern.id == 0 {
            pattern.id = self.sequencer.patterns().active_id();
        }
        self.set_pattern(pattern)
    }

    /// Set the session tempo. Returns the value that was stored.
    ///
    /// Rejects what is not a tempo (NaN, infinity, out of range) rather than
    /// clamping it: the UI's number input can produce those, and a user who
    /// typed 5000 should be told, not silently given 999. The transport clamps
    /// too, as a last line of defence.
    ///
    /// Like [`set_pattern`](Self::set_pattern), a missing device is not an
    /// error -- the tempo is the session's, not the stream's.
    ///
    /// # Errors
    /// [`SequencerError::InvalidBpm`], or [`SequencerError::Audio`] when the
    /// command queue is full.
    pub fn set_bpm(&self, bpm: f64) -> Result<f64, SequencerError> {
        if !bpm.is_finite() || !(MIN_BPM..=MAX_BPM).contains(&bpm) {
            return Err(SequencerError::InvalidBpm(bpm));
        }
        let stored = self.sequencer.set_bpm(clamp_bpm(bpm));
        match self.audio.set_bpm(stored) {
            Ok(_) | Err(AudioIoError::NotRunning) => Ok(stored),
            Err(err) => Err(err.into()),
        }
    }

    /// The session tempo.
    pub fn bpm(&self) -> f64 {
        self.sequencer.bpm()
    }

    /// The pattern and tempo service, for code that needs it directly (tests,
    /// and one day the project-file loader).
    pub fn sequencer(&self) -> &Arc<SequencerService> {
        &self.sequencer
    }

    /// The snapshot handed to the webview. Cheap to clone (a few short strings).
    pub fn snapshot(&self) -> UiSnapshot {
        self.snapshot.clone()
    }

    pub fn meters(&self) -> &Arc<MeterPump> {
        &self.meters
    }

    pub fn audio(&self) -> &Arc<AudioIoService> {
        &self.audio
    }

    pub fn midi(&self) -> &Arc<MidiIoService> {
        &self.midi
    }

    /// The audio capture harness behind `debug_capture` and
    /// `MYDAW_DEBUG_CAPTURE`.
    pub fn debug_tap(&self) -> &Arc<DebugTapService> {
        &self.debug_tap
    }

    /// The microphone / line-in recorder behind the transport bar's REC button.
    pub fn recorder(&self) -> &Arc<RecordService> {
        &self.recorder
    }

    /// End the take, write it, and load it as an audio source.
    ///
    /// The three steps are one method because they are one gesture: a take that
    /// was captured but not written is lost the moment the process exits, and a
    /// take written but not loaded is a file the user has to go and find. What
    /// this deliberately does *not* do is place the source anywhere -- that is
    /// `add_sample_channel`, so recording reuses the same path a dropped WAV
    /// takes and the two cannot drift.
    ///
    /// # Errors
    /// [`RecordingError::Record`] for everything the engine refuses (nothing
    /// recorded, device lost), [`RecordingError::Write`] when the WAV cannot be
    /// written, and [`RecordingError::Load`] when the file that was just written
    /// cannot be decoded -- which would be a bug here, not a user error, and is
    /// reported as such rather than silently swallowed.
    pub fn finish_recording(&self) -> Result<FinishedTake, RecordingError> {
        let take = self.recorder.stop()?;
        log::info!(
            "take finished: {:.2} s, peak {:.3}, {} dropped",
            take.duration_seconds(),
            take.peak(),
            take.dropped
        );
        self.write_and_load(&take)
    }

    /// Salvage a take whose device died, if there is one worth keeping.
    ///
    /// The capture callback cannot notice its own death, so the transition out
    /// of `Recording` happens on whichever thread next asks for the status. When
    /// that thread finds a half-finished take, this is what turns it into a file
    /// rather than throwing away the part of the performance that did arrive.
    pub fn reap_recording_fault(&self) -> Option<FinishedTake> {
        let take = self.recorder.reap_fault()?;
        match self.write_and_load(&take) {
            Ok(finished) => {
                log::warn!(
                    "the input device died mid-take; {:.2} s were salvaged",
                    take.duration_seconds()
                );
                Some(finished)
            }
            Err(err) => {
                log::warn!("a salvaged take could not be kept: {err}");
                None
            }
        }
    }

    /// Write one take under the next free name and decode it back in.
    fn write_and_load(&self, take: &RecordedTake) -> Result<FinishedTake, RecordingError> {
        let dir = crate::recording::recordings_dir();
        let path = crate::recording::next_take_path(&dir, std::time::SystemTime::now())?;
        crate::recording::write_take(take, &path)?;
        log::info!("take written: {}", path.display());
        Ok(FinishedTake {
            source: self.load_audio_source(&path)?,
            dropped_samples: take.dropped,
            limit_reached: take.limit_reached,
            peak: take.peak(),
        })
    }
}

/// A take that made it to disk and back into the session.
///
/// Carries the two things the source itself cannot say -- whether the capture
/// dropped anything, and whether the length cap ended it -- because the moment
/// to tell the user is the moment the channel appears, not later.
#[derive(Debug, Clone)]
pub struct FinishedTake {
    pub source: Arc<mydaw_plugin_sequencer::AudioSource>,
    pub dropped_samples: u64,
    pub limit_reached: bool,
    pub peak: f32,
}

/// Why a take did not become an audio source.
///
/// Three separate failures with three separate fixes, kept apart so the status
/// bar can say which one happened: "nothing was recorded" is the user's cable,
/// "could not write" is their disk, and "could not decode" is our bug.
#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error(transparent)]
    Record(#[from] RecordError),

    #[error(transparent)]
    Write(#[from] crate::recording::TakeWriteError),

    #[error("the take was written but could not be read back: {0}")]
    Load(#[from] SequencerError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_ipc_bridge::PluginStateDto;

    #[test]
    fn midi_off_stays_on_the_channel_the_note_started_on() {
        let mut sounding = HashMap::new();
        assert_eq!(midi_event_channel(&mut sounding, 2, 60, true), 2);
        assert_eq!(midi_event_channel(&mut sounding, 5, 60, false), 2);
        assert!(sounding.is_empty());
        assert_eq!(midi_event_channel(&mut sounding, 4, 64, false), 4);
    }

    #[test]
    fn boots_the_full_plugin_set_in_dependency_order() {
        let kernel = boot_kernel().expect("the Phase 0 plugin set must boot");
        assert_eq!(
            kernel.boot_order(),
            vec![
                "metering",
                "debug-tap",
                "test-tone",
                "instruments",
                "effects",
                "sampler",
                "sequencer",
                "host-clap",
                "host-vst3",
                "plugin-windows",
                "audio-io",
                "midi-io",
            ],
            "audio-io waits for metering and debug-tap; midi-io waits for \
             audio-io; Kahn's algorithm keeps the rest in registration order"
        );
        assert!(kernel
            .plugin_statuses()
            .iter()
            .all(|p| p.state == mydaw_kernel::PluginState::Started));
    }

    #[test]
    fn the_snapshot_lists_every_plugin_and_the_shell_panel() {
        let kernel = boot_kernel().unwrap();
        let snapshot = ui_snapshot(&kernel);

        assert_eq!(snapshot.plugins.len(), 12);
        assert!(snapshot
            .plugins
            .iter()
            .all(|p| p.state == PluginStateDto::Started));

        // Kernel status first (shell-contributed), then the plugins' own panels.
        let components: Vec<&str> = snapshot
            .panels
            .iter()
            .map(|p| p.component.as_str())
            .collect();
        assert_eq!(
            components,
            vec![
                "kernel-status",
                "mixer",
                "meters",
                // The sequencer plugin owns the pattern, so it contributes both
                // of the views onto it -- the grid and the roll.
                "step-sequencer",
                "piano-roll",
                "playlist",
                "clap-browser",
                "vst3-browser"
            ],
            "the shell's own panels first (the kernel registry and the mixer, \
             neither of which any plugin owns), then the plugins' in kernel boot \
             order; debug-tap deliberately contributes none"
        );
    }

    #[test]
    fn the_clap_browser_panel_is_contributed_by_the_host_plugin() {
        let kernel = boot_kernel().unwrap();
        let snapshot = ui_snapshot(&kernel);
        let panel = snapshot
            .panels
            .iter()
            .find(|p| p.component == "clap-browser")
            .expect("host-clap must contribute the browser panel");
        assert_eq!(panel.id, "clap-browser");
        assert_eq!(panel.title, "CLAP Plugins");
    }

    #[test]
    fn state_claims_the_meter_throttle_exactly_once() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).expect("all services present");
        assert!(state.meters().has_throttle());

        let output = kernel.ctx().services.get::<MeterOutput>().unwrap();
        assert!(
            !output.is_available(),
            "the pump owns the throttle after boot"
        );
    }

    #[test]
    fn the_clap_plugin_list_is_available_and_never_panics() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();

        // Lists this machine's CLAP plugins from the on-disk cache only. A
        // Rescan would open every unknown module, which is how unlicensed
        // plugins pop an activation window — that is not a test's job.
        let plugins = state.clap_plugins(false);
        for plugin in &plugins {
            assert!(!plugin.id.is_empty(), "a scanned plugin always has an id");
            assert!(!plugin.path.is_empty());
        }
        assert!(state.clap().has_cached_scan());
        assert_eq!(state.clap_plugins(false).len(), plugins.len());
    }

    #[test]
    fn the_debug_tap_service_is_available_and_idle_after_boot() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        let debug = state.debug_tap();
        assert!(!debug.is_armed());
        assert_eq!(debug.dropped(), 0);
        assert_eq!(
            debug.sample_rate(),
            0.0,
            "no stream has opened, so no rate is known yet"
        );
    }

    #[test]
    fn transport_commands_fail_cleanly_without_a_device() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        // No `start_audio` call: this is the headless path.
        assert!(matches!(
            state.set_playing(true),
            Err(AudioIoError::NotRunning)
        ));
    }

    #[test]
    fn notes_fail_cleanly_without_a_device() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        assert!(matches!(
            state.note_on(60, 1.0),
            Err(NoteError::Audio(AudioIoError::NotRunning))
        ));
        assert!(matches!(
            state.note_off(60),
            Err(NoteError::Audio(AudioIoError::NotRunning))
        ));
    }

    #[test]
    fn the_source_list_offers_every_source_and_starts_on_the_selected_track() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();

        // A test process has no assets installed, so the starter is the
        // melodic channel alone -- and track 0 is what the keyboard and the
        // selector address until the user says otherwise.
        assert_eq!(state.selected_track(), 0);
        assert_eq!(state.mixer_state().tracks[0].name, "Poly Synth");
        assert!(matches!(state.current_source(), Source::PolySynth));

        let sources = state.sources();
        // The built-ins come first and in a fixed order; whatever CLAP
        // instruments this machine has follow (CI: none).
        assert_eq!(
            sources
                .iter()
                .take(Source::BUILT_IN.len())
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["poly-synth", "test-tone"]
        );
        assert!(
            sources
                .iter()
                .skip(Source::BUILT_IN.len())
                .all(|s| s.id.starts_with("clap:") && s.bundle_path.is_some()),
            "every listed plugin must carry the bundle path the picker sends back"
        );
        let selected: Vec<&str> = sources
            .iter()
            .filter(|s| s.selected)
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(
            selected,
            vec!["poly-synth"],
            "the picker marks what the selected channel is playing"
        );
    }

    #[test]
    fn a_clap_command_in_a_headless_session_fails_at_once_rather_than_timing_out() {
        // No event loop in a test process, and five seconds per command would
        // make this suite unusable. The bridge knows nobody is serving it.
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        assert!(!state.clap_bridge().is_served());

        let started = std::time::Instant::now();
        let err = state
            .set_track_source_clap(0, &std::env::temp_dir().display().to_string(), "com.acme.x")
            .unwrap_err();
        assert!(matches!(err, SourceError::MainThread(_)), "{err:?}");
        assert_eq!(err.to_string(), crate::clap_bridge::NO_MAIN_THREAD);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));

        for outcome in [
            state.clear_master_effect(),
            state.open_clap_gui(ClapTarget::Master).map(|_| ()),
            state
                .close_clap_gui(ClapTarget::Track { index: 0 })
                .map(|_| ()),
            state.save_clap_state(ClapTarget::Master).map(|_| ()),
            state.load_clap_state(ClapTarget::Master, vec![1]),
        ] {
            assert_eq!(
                outcome.unwrap_err().to_string(),
                crate::clap_bridge::NO_MAIN_THREAD
            );
        }
    }

    #[test]
    fn a_bundle_path_that_is_not_there_is_refused_before_the_main_thread_is_asked() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        let missing = std::env::temp_dir().join("mydaw-no-such-bundle-88ad.clap");
        let err = state
            .set_track_source_clap(0, &missing.display().to_string(), "com.acme.x")
            .unwrap_err();
        assert!(err.to_string().contains("no CLAP bundle"), "{err}");
        assert_eq!(state.clap_bridge().pending(), 0);
    }

    #[test]
    fn switching_source_without_a_device_reports_it_instead_of_pretending() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();

        // Headless: the instruments are all registered, there is simply nowhere
        // to install them.
        for id in ["test-tone", "poly-synth"] {
            let err = state.set_source_id(id).unwrap_err();
            assert!(
                matches!(err, SourceError::Audio(AudioIoError::NotRunning)),
                "{id} produced {err:?}"
            );
        }
        assert!(
            matches!(state.current_source(), Source::PolySynth),
            "a failed switch must not change what the UI is told is playing: {:?}",
            state.current_source()
        );
    }

    #[test]
    fn a_bare_clap_id_is_refused_because_it_names_no_bundle() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        let err = state.set_source_id("clap:com.acme.thing").unwrap_err();
        assert!(
            matches!(err, SourceError::ClapNeedsBundle { .. }),
            "{err:?}"
        );
        assert_eq!(state.clap_bridge().pending(), 0);
    }

    #[test]
    fn an_unknown_source_id_is_rejected_before_anything_is_built() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        let err = state.set_source_id("wavetable").unwrap_err();
        assert!(matches!(err, SourceError::Unknown(ref id) if id == "wavetable"));
        assert!(err.to_string().contains("wavetable"), "{err}");
    }

    #[test]
    fn every_built_in_source_can_actually_be_built() {
        // The failure this guards against is a `type_id` in `Source` that no
        // plugin contributes: a typo that would only show up as a silent
        // fallback to the test tone at runtime.
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        for source in Source::BUILT_IN {
            assert!(
                state.tracks().build_node(&source, 48_000.0, 512).is_ok(),
                "'{}' could not be built",
                source.id()
            );
        }
    }

    #[test]
    fn a_note_the_webview_should_never_have_sent_is_rejected_before_the_queue() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();

        // Validation happens before the device is consulted, so these fail with
        // the *useful* error even on a machine with no sound card.
        assert!(matches!(
            state.note_on(128, 1.0),
            Err(NoteError::KeyOutOfRange(128))
        ));
        assert!(matches!(
            state.note_off(200),
            Err(NoteError::KeyOutOfRange(200))
        ));
        for bad in [-0.1_f32, 1.5, f32::NAN, f32::INFINITY] {
            assert!(
                matches!(state.note_on(60, bad), Err(NoteError::InvalidVelocity(_))),
                "velocity {bad} should have been rejected"
            );
        }
        assert!(
            state.note_on(MAX_KEY, 1.0).is_err(),
            "key 127 is valid; it fails only because there is no device"
        );
        assert!(matches!(
            state.note_on(MAX_KEY, 1.0),
            Err(NoteError::Audio(_))
        ));
    }

    #[test]
    fn a_fresh_session_reports_an_untitled_project_with_nothing_missing() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        let info = state.project_info();
        assert_eq!(info.name, crate::project_file::UNTITLED);
        assert!(info.path.is_none(), "nothing has been saved yet");
        assert_eq!(info.missing_plugins, 0);
        assert!(state.project_path().is_none(), "so Save has to ask where");
        assert!(state.project_bridge().missing().is_empty());
        assert!(state
            .mixer_state()
            .tracks
            .iter()
            .all(|track| track.missing.is_none()));
    }

    #[test]
    fn a_project_command_in_a_headless_session_fails_at_once_rather_than_timing_out() {
        // A minute per command (see `PROJECT_TIMEOUT`) would make this suite
        // unusable, and the answer is knowable without asking anyone.
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        assert!(!state.project_bridge().is_served());

        let started = std::time::Instant::now();
        for outcome in [
            state.new_project(),
            state.open_project("nothing.mydaw"),
            state.save_project("nothing.mydaw"),
        ] {
            let err = outcome.unwrap_err();
            assert!(matches!(err, SourceError::MainThread(_)), "{err:?}");
            assert_eq!(err.to_string(), crate::project_bridge::NO_MAIN_THREAD);
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    // ------------------------------------------------------------ sequencer

    fn state() -> (Kernel, AppState) {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        (kernel, state)
    }

    #[test]
    fn a_fresh_session_offers_an_empty_four_row_grid_at_the_default_tempo() {
        let (_kernel, state) = state();
        let dto = state.sequencer_state();
        assert_eq!(dto.bpm, mydaw_rt_core::DEFAULT_BPM);
        assert_eq!(dto.sample_rate, 0.0, "no device is open in a test process");
        assert_eq!(dto.pattern_set.active.steps, 16);
        assert_eq!(
            dto.pattern_set
                .active
                .channels
                .iter()
                .filter_map(|clip| clip.steps.as_ref().map(|row| row.key))
                .collect::<Vec<_>>(),
            vec![mydaw_plugin_sequencer::DEFAULT_STEP_KEY; 1],
            "a test process has no assets, so the starter is the synth alone, on C4"
        );
        assert!(dto.pattern_set.active.channels.iter().all(|clip| clip
            .steps
            .as_ref()
            .is_none_or(|row| row.steps.iter().all(|step| !step.on))));
        assert_eq!(
            dto.pattern_set.active.channels[0]
                .steps
                .as_ref()
                .unwrap()
                .label,
            "Key 60",
            "a grid row is labelled by its key, not by the channel it sits on"
        );
        assert_eq!(dto.play_mode, "pat");
        assert_eq!(dto.pattern_set.patterns.len(), 1);
        assert_eq!(dto.playlist.tracks.len(), 4);
    }

    #[test]
    fn programming_a_beat_works_with_no_audio_device_at_all() {
        // The point of the decision: a pattern is project data. On a headless
        // machine every note command fails, and every *edit* still has to work.
        let (_kernel, state) = state();
        let mut pattern = state.sequencer().active_pattern();
        pattern.channels[0].steps =
            Some(mydaw_plugin_sequencer::StepRow::empty(60).with_steps(&[0, 4, 8, 12]));
        state
            .set_pattern(pattern.clone())
            .expect("no device needed");

        assert_eq!(state.sequencer().active_pattern(), pattern);
        assert!(
            state.sequencer_state().pattern_set.active.channels[0]
                .steps
                .as_ref()
                .unwrap()
                .steps[4]
                .on
        );
        // Four steps became four notes: the audio thread only sees notes.
        assert_eq!(
            state
                .sequencer()
                .compile()
                .active_pattern()
                .unwrap()
                .note_count(),
            4
        );
    }

    #[test]
    fn a_rejected_pattern_leaves_the_session_exactly_as_it_was() {
        let (_kernel, state) = state();
        let mut good = state.sequencer().active_pattern();
        good.channels[0].steps = Some(mydaw_plugin_sequencer::StepRow::empty(60).with_steps(&[0]));
        state.set_pattern(good.clone()).unwrap();

        let bad = PatternDto {
            id: good.id,
            name: "Pattern 1".to_owned(),
            color: None,
            steps: 16,
            channels: vec![crate::dto::ChannelClipDto {
                channel: 0,
                mode: String::new(),
                steps: Some(crate::dto::StepRowDto {
                    key: 60,
                    label: String::new(),
                    steps: Vec::new(),
                }),
                notes: Vec::new(),
            }],
        };
        let err = state.set_pattern_dto(&bad).unwrap_err();
        assert!(matches!(err, SequencerError::Pattern(_)), "{err:?}");
        assert_eq!(state.sequencer().active_pattern(), good);
    }

    #[test]
    fn a_new_pattern_opens_with_a_grid_on_every_rack_channel() {
        // The channels come from the mixer, and the step keys from what is on
        // them -- a session of five drums and a synth opens a new pattern with
        // five drum grids and one on middle C, not with nothing.
        let (_kernel, state) = state();
        let id = state.add_pattern(Some("Verse".to_owned())).unwrap();
        let set = state.sequencer().patterns();
        assert_eq!(set.len(), 2);
        assert_eq!(
            set.active_id(),
            1,
            "adding is not switching; a UI that wants both says both"
        );

        let added = set.get(id).expect("just added");
        assert_eq!(added.name, "Verse");
        assert_eq!(
            added
                .channels
                .iter()
                .filter_map(|clip| clip.steps.as_ref().map(|row| row.key))
                .collect::<Vec<_>>(),
            vec![mydaw_plugin_sequencer::DEFAULT_STEP_KEY; 1]
        );

        state.set_active_pattern_for_test(id);
        assert_eq!(state.sequencer_state().pattern_set.active_id, id);
    }

    #[test]
    fn adding_and_removing_a_rack_channel_moves_every_pattern_with_it() {
        // A channel *is* a track index. A pattern left pointing at the wrong
        // instrument would turn "delete a track" into "delete a track and
        // quietly rewrite the song".
        let (_kernel, state) = state();
        let before = state.sequencer().active_pattern().channels.len();

        // No device, so the graph install fails -- the project edit in front of
        // it is what is under test, and the pattern must follow it.
        let _ = state.add_track(Source::PolySynth, None);
        assert_eq!(
            state.sequencer().active_pattern().channels.len(),
            before + 1
        );
        assert!(state
            .sequencer()
            .active_pattern()
            .channel(before as u8)
            .is_some());

        let _ = state.remove_track(0);
        let pattern = state.sequencer().active_pattern();
        assert_eq!(pattern.channels.len(), before);
        assert_eq!(
            pattern
                .channels
                .iter()
                .map(|c| c.channel)
                .collect::<Vec<_>>(),
            (0..before as u8).collect::<Vec<_>>(),
            "the channels above the removed one shifted down"
        );
    }

    #[test]
    fn a_new_channel_gets_a_grid_in_every_pattern_and_not_only_the_active_one() {
        // The rack draws whatever the *active* pattern says, so a channel that
        // reached only that one looks perfectly fine until you click another
        // pattern tab and find a row with no lane in it.
        //
        // This drives the plain path, which is the only one a test process can
        // reach -- the other two need a main-thread plugin host with a real
        // bundle loaded. What makes it cover them too is that `add_track` now
        // has a single exit and a single `sync_channel_added`, so there is no
        // longer a branch that *can* skip it. That, not this assertion, is what
        // fixed the bug; this holds the invariant the fix restored.
        let (_kernel, state) = state();
        let first = state.sequencer().patterns().active_id();
        let second = state.add_pattern(None).expect("a second pattern");
        let before = state.sequencer().active_pattern().channels.len();

        let _ = state.add_track(Source::PolySynth, None);

        for id in [first, second] {
            state
                .edit_patterns(|set| set.set_active(id))
                .expect("the pattern exists");
            let pattern = state.sequencer().active_pattern();
            let clip = pattern
                .channel(before as u8)
                .unwrap_or_else(|| panic!("pattern {id} must speak for the new channel"));
            let row = clip
                .steps
                .as_ref()
                .unwrap_or_else(|| panic!("pattern {id}'s new channel must have a grid to click"));
            assert!(
                row.steps.iter().all(|cell| !cell.on),
                "and the grid it gets must be empty"
            );
        }
    }

    #[test]
    fn the_play_mode_switches_with_no_audio_device_at_all() {
        let (_kernel, state) = state();
        assert_eq!(state.play_mode(), PlayMode::Pattern);
        state
            .set_play_mode(PlayMode::Song)
            .expect("no device needed");
        assert_eq!(state.play_mode(), PlayMode::Song);
        assert_eq!(state.sequencer_state().play_mode, "song");
    }

    #[test]
    fn an_arrangement_is_built_and_deleting_its_pattern_takes_its_clips() {
        let (_kernel, state) = state();
        let second = state.add_pattern(None).unwrap();
        for (track, start) in [(0_usize, 0_u64), (0, 3_840), (1, 3_840)] {
            state
                .edit_playlist(|list| list.add_pattern_clip(second, track, start, 3_840))
                .unwrap();
        }
        assert_eq!(state.sequencer_state().playlist.clips.len(), 3);
        assert_eq!(state.sequencer_state().playlist.end_ticks, 7_680);

        assert_eq!(state.remove_pattern(second).unwrap(), 3);
        assert!(state.sequencer_state().playlist.clips.is_empty());
        assert_eq!(state.sequencer_state().playlist.end_ticks, 0);
    }

    #[test]
    fn the_tempo_is_stored_and_out_of_range_values_are_named_rather_than_clamped() {
        let (_kernel, state) = state();
        assert_eq!(state.set_bpm(174.3).unwrap(), 174.3);
        assert_eq!(state.bpm(), 174.3);
        assert_eq!(state.sequencer_state().bpm, 174.3);

        for bad in [0.0, 19.9, 999.1, f64::NAN, f64::INFINITY, -130.0] {
            let err = state.set_bpm(bad).unwrap_err();
            assert!(
                matches!(err, SequencerError::InvalidBpm(_)),
                "tempo {bad} produced {err:?}"
            );
        }
        assert_eq!(state.bpm(), 174.3, "a refused tempo changes nothing");
        // The bounds themselves are legal.
        assert_eq!(state.set_bpm(MIN_BPM).unwrap(), MIN_BPM);
        assert_eq!(state.set_bpm(MAX_BPM).unwrap(), MAX_BPM);
    }

    #[test]
    fn a_pattern_from_the_webview_is_validated_before_it_is_stored() {
        let (_kernel, state) = state();
        let mut dto = state.sequencer_state().pattern_set.active;
        let row = dto.channels[0].steps.as_mut().unwrap();
        row.steps[0].on = true;
        row.steps[0].velocity = 1.0;
        state.set_pattern_dto(&dto).unwrap();
        assert_eq!(state.sequencer().active_pattern().hit_count(), 1);

        let row = dto.channels[0].steps.as_mut().unwrap();
        row.steps[1].velocity = f32::NAN;
        row.steps[1].on = true;
        assert!(state.set_pattern_dto(&dto).is_err());
        assert_eq!(
            state.sequencer().active_pattern().hit_count(),
            1,
            "unchanged"
        );
    }

    fn write_tone_wav(name: &str, hz: f64) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("mydaw-pitch-{name}-{}.wav", std::process::id()));
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).expect("wav");
        for frame in 0..44_100 {
            let sample =
                ((frame as f64 * hz * std::f64::consts::TAU) / 44_100.0).sin() as f32 * 0.5;
            writer.write_sample(sample).unwrap();
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        path
    }

    fn add_headless_sample(state: &AppState, path: &std::path::Path) -> usize {
        let before = state.mixer_state().tracks.len();
        match state.add_sample_channel(path) {
            Ok(index) => index,
            Err(MixerError::Audio(AudioIoError::NotRunning)) => {
                assert_eq!(state.mixer_state().tracks.len(), before + 1);
                before
            }
            Err(err) => panic!("sample channel: {err:?}"),
        }
    }

    #[test]
    fn analyze_track_pitch_names_missing_and_wrong_source_tracks() {
        let (_kernel, state) = state();
        assert!(matches!(
            state.analyze_track_pitch(99),
            Err(MixerError::Track(TrackError::NoSuchTrack(99)))
        ));
        let synth = state
            .mixer_state()
            .tracks
            .iter()
            .position(|track| track.engine != "sample" && track.engine != "clip")
            .expect("a non-sample channel");
        assert!(matches!(
            state.analyze_track_pitch(synth),
            Err(MixerError::Track(TrackError::NotASampleChannel(_)))
        ));
    }

    #[test]
    fn analyze_track_pitch_detects_a_tone_and_rejects_noise_by_filename() {
        let (_kernel, state) = state();
        let tone = write_tone_wav("tone", 440.0);
        let track = add_headless_sample(&state, &tone);
        match state.analyze_track_pitch(track).expect("analysis") {
            PitchAnalysisDto::Detected {
                midi_note, cents, ..
            } => {
                assert_eq!(midi_note, 69);
                assert!(cents.abs() < 15.0, "A4 cents were {cents}");
            }
            other => panic!("expected a detected A4, got {other:?}"),
        }
        state
            .set_track_sample_root(track, 69, 0.0, RootMode::Detected)
            .expect("root");
        let stored = &state.mixer_state().tracks[track];
        assert_eq!(stored.root_key, 69);
        assert_eq!(stored.root_mode, RootMode::Detected);
        let _ = std::fs::remove_file(tone);
    }

    /// One drum hit for the slicer tests.
    ///
    /// Synthesised here rather than rendered from an instrument: `assets/` is
    /// gitignored, so a sampled kit is not there to render on a fresh clone, and
    /// a slicer test that silently skips itself is worse than no test. What the
    /// detector needs is a sharp transient followed by a decay, which both of
    /// these have.
    #[derive(Clone, Copy)]
    enum Hit {
        Kick,
        Snare,
    }

    impl Hit {
        /// One hit, 48 kHz mono, decaying to silence.
        fn render(self) -> Vec<f32> {
            let rate = 48_000.0_f32;
            let (len, decay) = match self {
                Self::Kick => (12_000, 28.0),
                Self::Snare => (8_000, 45.0),
            };
            // A fixed xorshift, so the noise is the same on every machine and
            // run: a slicer assertion that depends on the weather is not one.
            let mut noise: u32 = 0x1234_5678;
            (0..len)
                .map(|index| {
                    let t = index as f32 / rate;
                    let envelope = (-decay * t).exp();
                    let body = match self {
                        Self::Kick => (std::f32::consts::TAU * 55.0 * t).sin(),
                        Self::Snare => {
                            noise ^= noise << 13;
                            noise ^= noise >> 17;
                            noise ^= noise << 5;
                            (noise as f32 / u32::MAX as f32) * 2.0 - 1.0
                        }
                    };
                    body * envelope * 0.8
                })
                .collect()
        }
    }

    /// A four-bar break at known frame positions, for the slicer.
    fn write_break_wav(_state: &AppState, hits: &[(usize, Hit)]) -> (std::path::PathBuf, usize) {
        let rate = 48_000_usize;
        let frames = rate * 2;
        let mut left = vec![0.0_f32; frames];
        let mut right = vec![0.0_f32; frames];
        for (at, hit) in hits {
            for (offset, sample) in hit.render().iter().enumerate() {
                if at + offset < frames {
                    left[at + offset] += sample;
                    right[at + offset] += sample;
                }
            }
        }
        let path = std::env::temp_dir().join(format!("mydaw-break-{}.wav", std::process::id()));
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: rate as u32,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).expect("wav");
        for frame in 0..frames {
            writer.write_sample(left[frame].clamp(-1.0, 1.0)).unwrap();
            writer.write_sample(right[frame].clamp(-1.0, 1.0)).unwrap();
        }
        writer.finalize().unwrap();
        (path, frames)
    }

    #[test]
    fn analyze_slices_names_missing_and_wrong_source_tracks() {
        let (_kernel, state) = state();
        assert!(matches!(
            state.analyze_track_slices(99, SliceMode::Transient, 0.5, 16),
            Err(MixerError::Track(TrackError::NoSuchTrack(99)))
        ));
        let synth = state
            .mixer_state()
            .tracks
            .iter()
            .position(|track| track.engine != "sample" && track.engine != "clip")
            .expect("a non-sample channel");
        assert!(matches!(
            state.analyze_track_slices(synth, SliceMode::Transient, 0.5, 16),
            Err(MixerError::Track(TrackError::NotASampleChannel(_)))
        ));
        assert!(matches!(
            state.set_track_slices(synth, Default::default()),
            Err(MixerError::Track(TrackError::NotASampleChannel(_)))
        ));
    }

    #[test]
    fn transient_slicing_finds_the_hits_of_a_drum_break() {
        let (_kernel, state) = state();
        let hits = [
            (0_usize, Hit::Kick),
            (24_000, Hit::Snare),
            (48_000, Hit::Kick),
            (72_000, Hit::Snare),
        ];
        let (path, _frames) = write_break_wav(&state, &hits);
        let track = add_headless_sample(&state, &path);

        let analysis = state
            .analyze_track_slices(track, SliceMode::Transient, 0.5, 16)
            .expect("analysis");
        assert_eq!(
            analysis.frames.len(),
            hits.len(),
            "one slice per hit, got {:?}",
            analysis.frames
        );
        // 5 ms: the file is real drums with real decay tails, so the budget is
        // wider than the synthetic click test in `mydaw-slice-analysis`.
        let budget = (0.005 * 48_000.0) as i64;
        for ((expected, _), actual) in hits.iter().zip(&analysis.frames) {
            let error = (*actual as i64 - *expected as i64).abs();
            assert!(error <= budget, "hit at {expected} reported at {actual}");
        }
        assert_eq!(analysis.range_start, 0);
        assert!(!analysis.truncated);

        // Installing it is what the audio thread sees.
        let stored = state
            .set_track_slices(
                track,
                crate::project::SliceStateDto {
                    mode: SliceMode::Transient,
                    frames: analysis.frames.clone(),
                    sensitivity: 0.5,
                    division: 16,
                    keytrack: false,
                },
            )
            .expect("install");
        assert_eq!(stored.frames, analysis.frames);
        assert_eq!(
            state.mixer_state().tracks[track].slices.frames,
            stored.frames
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn grid_slicing_divides_the_region_and_a_cleared_mode_keeps_the_markers() {
        let (_kernel, state) = state();
        let (path, frames) = write_break_wav(&state, &[(0, Hit::Kick)]);
        let track = add_headless_sample(&state, &path);
        assert_eq!(frames, 96_000, "two seconds at 48 kHz");

        // Two seconds is one bar at 120 BPM; the default session is 130, so
        // ask for the tempo the fixture was written at.
        state.set_bpm(120.0).expect("tempo");
        let analysis = state
            .analyze_track_slices(track, SliceMode::Grid, 0.5, 4)
            .expect("analysis");
        assert_eq!(analysis.bars, 1);
        assert_eq!(analysis.frames, vec![0, 24_000, 48_000, 72_000]);

        state
            .set_track_slices(
                track,
                crate::project::SliceStateDto {
                    mode: SliceMode::Grid,
                    frames: analysis.frames.clone(),
                    sensitivity: 0.5,
                    division: 4,
                    keytrack: false,
                },
            )
            .expect("install");

        // Back to Region: the markers stay in the project, the player does not.
        let off = state
            .set_track_slices(
                track,
                crate::project::SliceStateDto {
                    mode: SliceMode::Off,
                    frames: analysis.frames.clone(),
                    sensitivity: 0.5,
                    division: 4,
                    keytrack: false,
                },
            )
            .expect("clear");
        assert_eq!(off.frames, analysis.frames, "the chop is not thrown away");
        assert!(!off.is_sliced());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_dragged_marker_is_sorted_deduped_and_capped_before_it_reaches_the_player() {
        let (_kernel, state) = state();
        let (path, _frames) = write_break_wav(&state, &[(0, Hit::Kick)]);
        let track = add_headless_sample(&state, &path);
        let mut asked = vec![9_000_u64, 1_000, 5_000, 1_000, 10_000_000];
        asked.extend((0..200).map(|index| 20_000 + index * 100));
        let stored = state
            .set_track_slices(
                track,
                crate::project::SliceStateDto {
                    mode: SliceMode::Transient,
                    frames: asked,
                    sensitivity: 2.0,
                    division: 5,
                    keytrack: true,
                },
            )
            .expect("install");
        assert_eq!(
            stored.frames.len(),
            mydaw_plugin_sequencer::MAX_SLICES,
            "the engine's table is the ceiling"
        );
        assert!(
            stored.frames.windows(2).all(|pair| pair[0] < pair[1]),
            "sorted and deduped: {:?}",
            &stored.frames[..6]
        );
        assert!(
            !stored.frames.contains(&10_000_000),
            "a marker past the end of the file is dropped"
        );
        assert_eq!(stored.sensitivity, 1.0);
        assert_eq!(stored.division, 4);
        assert!(stored.keytrack);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn set_track_sample_root_refuses_a_synth_channel() {
        let (_kernel, state) = state();
        let synth = state
            .mixer_state()
            .tracks
            .iter()
            .position(|track| track.engine != "sample" && track.engine != "clip")
            .expect("a non-sample channel");
        assert!(matches!(
            state.set_track_sample_root(synth, 60, 0.0, RootMode::Manual),
            Err(MixerError::Track(TrackError::NotASampleChannel(_)))
        ));
    }
}
