//! Kernel boot and the state the Tauri commands run against.

use std::sync::Arc;

use mydaw_ipc_bridge::{PanelManifestDto, UiSnapshot};
use mydaw_kernel::{Kernel, KernelError};
use mydaw_plugin_audio_io::{AudioIoError, AudioIoPlugin, AudioIoService, StreamInfo};
use mydaw_plugin_debug_tap::{DebugTapPlugin, DebugTapService};
use mydaw_plugin_host_clap::{ClapHostService, HostClapPlugin};
use mydaw_plugin_host_vst3::{HostVst3Plugin, Vst3HostService};
use mydaw_plugin_instruments::{InstrumentRegistry, InstrumentsPlugin};
use mydaw_plugin_metering::{MeterOutput, MeteringPlugin};
use mydaw_plugin_sequencer::{Pattern, PatternError, SequencerPlugin, SequencerService};
use mydaw_plugin_test_tone::{TestToneFactory, TestTonePlugin};
use mydaw_plugin_windows::{PluginWindowService, PluginWindowsPlugin};

use mydaw_rt_core::{clamp_bpm, LiveNote, MAX_BPM, MAX_KEY, MIN_BPM};

use crate::clap_bridge::{ClapAnswer, ClapCommand, ClapGuiStatusDto, ClapHostBridge, ClapTarget};
use crate::dto::{ClapPluginInfoDto, PatternDto, SequencerStateDto, Vst3PluginInfoDto};
use crate::ipc_bench;
use crate::meters::MeterPump;
use crate::project::{MixerStateDto, Project, TrackError};
use crate::project_bridge::{ProjectBridge, ProjectCommand, ProjectInfoDto};
use crate::source::{check_bundle_path, Source, SourceDto, SourceError};
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
    #[error("tempo {0} is not a number between {MIN_BPM} and {MAX_BPM}")]
    InvalidBpm(f64),
    /// The audio thread has not picked up previous edits. Retryable.
    #[error(transparent)]
    Audio(#[from] AudioIoError),
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
    kernel.add_plugin(Box::new(SequencerPlugin::new()))?;
    kernel.add_plugin(Box::new(AudioIoPlugin::new()))?;
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
    /// The authoritative pattern and tempo. The audio thread runs copies of
    /// both; this is the original (and, one day, what a project file holds).
    sequencer: Arc<SequencerService>,
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
    /// The audio inspection harness. Non-RT, `Send + Sync`, and safe to call
    /// from a Tauri command thread.
    debug_tap: Arc<DebugTapService>,
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
        let sequencer = services
            .get::<SequencerService>()
            .ok_or(StartupError::MissingService("SequencerService"))?;
        let clap = services
            .get::<ClapHostService>()
            .ok_or(StartupError::MissingService("ClapHostService"))?;
        let vst3 = services
            .get::<Vst3HostService>()
            .ok_or(StartupError::MissingService("Vst3HostService"))?;
        let meter_output = services
            .get::<MeterOutput>()
            .ok_or(StartupError::MissingService("MeterOutput"))?;
        let debug_tap = services
            .get::<DebugTapService>()
            .ok_or(StartupError::MissingService("DebugTapService"))?;
        // Resolved here so a missing plugin-windows registration is a boot error
        // rather than a mystery when the user first clicks "Open GUI".
        if !services.contains::<PluginWindowService>() {
            return Err(StartupError::MissingService("PluginWindowService"));
        }

        let throttle = meter_output.take();
        if throttle.is_none() {
            log::warn!("meter throttle was already claimed; the UI meters will stay silent");
        }

        Ok(Self {
            snapshot: ui_snapshot(kernel),
            audio,
            sequencer,
            tracks: Arc::new(TrackHost::new(tone, instruments)),
            clap_bridge: Arc::new(ClapHostBridge::new()),
            clap,
            vst3_bridge: Arc::new(Vst3HostBridge::new()),
            vst3,
            project_bridge: Arc::new(ProjectBridge::new()),
            meters: MeterPump::new(throttle),
            debug_tap,
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
        if let Err(err) = self.audio.install_pattern(self.sequencer.compiled()) {
            log::warn!("could not publish the pattern to the new stream: {err}");
        }
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
            return self
                .clap_command(ClapCommand::SetTrackBuiltIn {
                    track,
                    source_id: source.id(),
                })
                .map(|_| ());
        }
        if self.vst3_bridge.board().track(track).is_some() {
            return self
                .vst3_command(Vst3Command::SetTrackBuiltIn {
                    track,
                    source_id: source.id(),
                })
                .map(|_| ());
        }
        let id = source.id();
        match self.tracks.set_source(track, source, &self.audio) {
            Ok(()) => {
                log::info!("track {track} source is now '{id}'");
                Ok(())
            }
            Err(MixerError::Source(err)) => Err(err),
            Err(MixerError::Audio(err)) => Err(SourceError::Audio(err)),
            // A bad index is a UI bug; it reaches the status bar as itself.
            Err(MixerError::Track(err)) => Err(SourceError::MainThread(err.to_string())),
        }
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
        sources.extend(
            self.clap
                .cached_scan()
                .iter()
                .filter(|info| info.is_instrument())
                .map(|info| SourceDto::clap(info, &current)),
        );
        sources
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

    /// The mixer, in the shape the webview reads.
    pub fn mixer_state(&self) -> MixerStateDto {
        self.tracks.state(
            self.clap_bridge.board(),
            self.vst3_bridge.board(),
            self.project_bridge.missing(),
        )
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

    /// Remove a track and rebuild the graph.
    ///
    /// See [`add_track`](Self::add_track) on which thread does the work.
    ///
    /// # Errors
    /// [`MixerError`] -- most usefully [`TrackError::LastTrack`].
    pub fn remove_track(&self, track: usize) -> Result<(), MixerError> {
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
        self.tracks.remove_track(track, &self.audio)
    }

    /// The CLAP host mailbox. Commands post here; the run loop serves it.
    pub fn clap_bridge(&self) -> &Arc<ClapHostBridge> {
        &self.clap_bridge
    }

    /// Installed CLAP plugins, in the shape the webview reads.
    ///
    /// Served from [`ClapHostService::cached_scan`]: the first call walks the
    /// standard plugin directories and loads every bundle it finds (slow, and it
    /// runs vendor code), later calls are free. `rescan` forces a fresh walk, for
    /// the case where the user installed a plugin without restarting MYDAW.
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
    /// # Errors
    /// [`AudioIoError::NotRunning`] when no device was opened.
    pub fn set_playing(&self, playing: bool) -> Result<(), AudioIoError> {
        self.audio.set_playing(playing)
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

    /// The tempo and pattern, in the shape the webview reads.
    pub fn sequencer_state(&self) -> SequencerStateDto {
        SequencerStateDto {
            bpm: self.sequencer.bpm(),
            sample_rate: self.audio.sample_rate().unwrap_or(0.0),
            pattern: PatternDto::from(&self.sequencer.pattern()),
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
        self.sequencer.set_pattern(pattern)?;
        match self.audio.install_pattern(self.sequencer.compiled()) {
            Ok(()) | Err(AudioIoError::NotRunning) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Replace the pattern from what the webview sent.
    ///
    /// # Errors
    /// Everything [`set_pattern`](Self::set_pattern) reports, plus
    /// [`SequencerError::Pattern`] for a DTO that does not describe a pattern
    /// at all (a row of the wrong length).
    pub fn set_pattern_dto(&self, dto: &PatternDto) -> Result<(), SequencerError> {
        self.set_pattern(Pattern::try_from(dto)?)
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

    /// The audio capture harness behind `debug_capture` and
    /// `MYDAW_DEBUG_CAPTURE`.
    pub fn debug_tap(&self) -> &Arc<DebugTapService> {
        &self.debug_tap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_ipc_bridge::PluginStateDto;

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
                "sequencer",
                "host-clap",
                "host-vst3",
                "plugin-windows",
                "audio-io",
            ],
            "audio-io waits for metering and debug-tap; Kahn's algorithm keeps \
             the rest in registration order"
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

        assert_eq!(snapshot.plugins.len(), 9);
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
                "step-sequencer",
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

        // Scans this machine's real CLAP directories. The count depends on what
        // is installed (CI: none), so the contract under test is "it returns a
        // well-formed list without panicking", plus the cache.
        let plugins = state.clap_plugins(false);
        for plugin in &plugins {
            assert!(!plugin.id.is_empty(), "a scanned plugin always has an id");
            assert!(!plugin.path.is_empty());
        }
        assert!(state.clap().has_cached_scan());
        assert_eq!(state.clap_plugins(false).len(), plugins.len());
        assert_eq!(
            state.clap_plugins(true).len(),
            plugins.len(),
            "a rescan of an unchanged machine finds the same plugins"
        );
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

        // Track 0 of the default project is the drum kit, and track 0 is what
        // the keyboard and the selector address until the user says otherwise.
        assert_eq!(state.selected_track(), 0);
        assert_eq!(state.current_source(), Source::DrumKit);
        let sources = state.sources();
        // The built-ins come first and in a fixed order; whatever CLAP
        // instruments this machine has follow (CI: none).
        assert_eq!(
            sources
                .iter()
                .take(3)
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["poly-synth", "drum-kit", "test-tone"]
        );
        assert!(
            sources
                .iter()
                .skip(3)
                .all(|s| s.id.starts_with("clap:") && s.bundle_path.is_some()),
            "every listed plugin must carry the bundle path the picker sends back"
        );
        let selected: Vec<&str> = sources
            .iter()
            .filter(|s| s.selected)
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(selected, vec!["drum-kit"], "exactly one is playing");

        // Selecting the synth track changes what the list marks, with no audio
        // device involved at all.
        state.set_selected_track(1).unwrap();
        assert_eq!(state.current_source(), Source::PolySynth);
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
        for id in ["drum-kit", "test-tone", "poly-synth"] {
            let err = state.set_source_id(id).unwrap_err();
            assert!(
                matches!(err, SourceError::Audio(AudioIoError::NotRunning)),
                "{id} produced {err:?}"
            );
        }
        assert_eq!(
            state.current_source(),
            Source::DrumKit,
            "a failed switch must not change what the UI is told is playing"
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
        assert_eq!(dto.pattern.steps, 16);
        assert_eq!(
            dto.pattern.rows.iter().map(|r| r.key).collect::<Vec<_>>(),
            vec![36, 38, 42, 46]
        );
        assert!(dto
            .pattern
            .rows
            .iter()
            .all(|row| row.steps.iter().all(|step| !step.on)));
        assert_eq!(dto.pattern.rows[2].label, "CH");
    }

    #[test]
    fn programming_a_beat_works_with_no_audio_device_at_all() {
        // The point of the decision: a pattern is project data. On a headless
        // machine every note command fails, and every *edit* still has to work.
        let (_kernel, state) = state();
        let mut pattern = state.sequencer().pattern();
        pattern.rows[0] = mydaw_plugin_sequencer::PatternRow::empty(36).with_steps(&[0, 4, 8, 12]);
        state
            .set_pattern(pattern.clone())
            .expect("no device needed");

        assert_eq!(state.sequencer().pattern(), pattern);
        assert!(state.sequencer_state().pattern.rows[0].steps[4].on);
        assert_eq!(state.sequencer().compiled().row_count(), 4);
    }

    #[test]
    fn a_rejected_pattern_leaves_the_session_exactly_as_it_was() {
        let (_kernel, state) = state();
        let mut good = state.sequencer().pattern();
        good.rows[0] = mydaw_plugin_sequencer::PatternRow::empty(36).with_steps(&[0]);
        state.set_pattern(good.clone()).unwrap();

        let bad = PatternDto {
            steps: 16,
            rows: vec![crate::dto::PatternRowDto {
                key: 36,
                channel: 0,
                label: String::new(),
                steps: Vec::new(),
            }],
        };
        let err = state.set_pattern_dto(&bad).unwrap_err();
        assert!(matches!(err, SequencerError::Pattern(_)), "{err:?}");
        assert_eq!(state.sequencer().pattern(), good);
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
        let mut dto = state.sequencer_state().pattern;
        dto.rows[0].steps[0].on = true;
        dto.rows[0].steps[0].velocity = 1.0;
        state.set_pattern_dto(&dto).unwrap();
        assert_eq!(state.sequencer().pattern().hit_count(), 1);

        dto.rows[0].steps[1].velocity = f32::NAN;
        dto.rows[0].steps[1].on = true;
        assert!(state.set_pattern_dto(&dto).is_err());
        assert_eq!(state.sequencer().pattern().hit_count(), 1, "unchanged");
    }
}
