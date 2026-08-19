//! Kernel boot and the state the Tauri commands run against.

use std::sync::Arc;

use mydaw_ipc_bridge::{PanelManifestDto, UiSnapshot};
use mydaw_kernel::{Kernel, KernelError};
use mydaw_plugin_audio_io::{AudioIoError, AudioIoPlugin, AudioIoService, StreamInfo};
use mydaw_plugin_debug_tap::{DebugTapPlugin, DebugTapService};
use mydaw_plugin_host_clap::{ClapHostService, HostClapPlugin, LoadedClapPlugin};
use mydaw_plugin_host_vst3::HostVst3Plugin;
use mydaw_plugin_instruments::{InstrumentRegistry, InstrumentsPlugin};
use mydaw_plugin_metering::{MeterOutput, MeteringPlugin};
use mydaw_plugin_sequencer::{Pattern, PatternError, SequencerPlugin, SequencerService};
use mydaw_plugin_test_tone::{TestToneFactory, TestTonePlugin};
use mydaw_plugin_windows::{PluginWindowService, PluginWindowsPlugin};

use mydaw_rt_core::{clamp_bpm, LiveNote, RtNode, RtNodeFactory, MAX_BPM, MAX_KEY, MIN_BPM};

use crate::dto::{ClapPluginInfoDto, PatternDto, SequencerStateDto};
use crate::graph::{clap_effect_node, clap_instrument_node, install_engine_graph};
use crate::gui_bridge::ClapGuiBridge;
use crate::ipc_bench;
use crate::meters::MeterPump;
use crate::source::{ClapSourceBridge, Source, SourceDto, SourceError, SourceHandle};

/// Panel key of the kernel status view, mirrored in `ui/src/panels/registry.ts`.
pub const KERNEL_STATUS_PANEL: &str = "kernel-status";

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
    tone: Arc<TestToneFactory>,
    /// The built-in instruments, by `type_id`.
    instruments: Arc<InstrumentRegistry>,
    /// The authoritative pattern and tempo. The audio thread runs copies of
    /// both; this is the original (and, one day, what a project file holds).
    sequencer: Arc<SequencerService>,
    /// The head of the chain. Swapping the source is a push through this, not a
    /// graph rebuild -- see `crate::source`.
    source: Arc<SourceHandle>,
    /// Mailbox to the main thread for the one source it alone can build.
    clap_source: Arc<ClapSourceBridge>,
    clap: Arc<ClapHostService>,
    meters: Arc<MeterPump>,
    /// The audio inspection harness. Non-RT, `Send + Sync`, and safe to call
    /// from a Tauri command thread.
    debug_tap: Arc<DebugTapService>,
    /// Mailbox to the main thread's plugin-GUI host. The editor itself cannot
    /// live here: it is `!Send` and thread-bound (see `crate::gui_host`).
    gui: Arc<ClapGuiBridge>,
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
            tone,
            instruments,
            sequencer,
            source: Arc::new(SourceHandle::new(Source::DEFAULT)),
            clap_source: Arc::new(ClapSourceBridge::new()),
            clap,
            meters: MeterPump::new(throttle),
            debug_tap,
            gui: Arc::new(ClapGuiBridge::new()),
        })
    }

    /// Open the output device and publish the test-tone graph.
    ///
    /// Callers treat failure as a warning, not a fatal error: a machine with no
    /// output device (headless CI, RDP session) must still get a working window.
    ///
    /// # Errors
    /// Whatever cpal reports, plus [`AudioIoError::NotRunning`] if the stream died
    /// between opening it and installing the graph.
    pub fn start_audio(&self) -> Result<StreamInfo, AudioIoError> {
        self.start_audio_with_plugins(None, None)
    }

    /// Open the output device and publish a chain headed by `source`.
    ///
    /// The one entry point the shell's startup path uses: `run()` picks the
    /// source (the CLAP instrument when one is configured, otherwise
    /// [`Source::DEFAULT`]), and everything after that is
    /// [`set_source`](Self::set_source).
    ///
    /// Both plugins are activated *after* the device is open, because
    /// activation needs the negotiated sample rate and block size. One that
    /// will not activate is logged and skipped -- the engine still runs.
    ///
    /// A source whose node cannot be built (a CLAP instrument that will not
    /// activate, a missing built-in) falls back to [`Source::DEFAULT`] with a
    /// warning rather than leaving the stream silent: a session with the wrong
    /// instrument is recoverable from the UI, and one with no graph at all is
    /// not.
    ///
    /// # Errors
    /// Whatever cpal reports, plus [`AudioIoError::NotRunning`] if the stream
    /// died between opening it and installing the graph.
    pub fn start_audio_with_source(
        &self,
        source: Source,
        instrument: Option<&mut LoadedClapPlugin>,
        effect: Option<&mut LoadedClapPlugin>,
    ) -> Result<StreamInfo, AudioIoError> {
        // Order is forced by the service: `install_graph` needs a live session,
        // because the graph channel is created together with the stream.
        let info = self.audio.start()?;
        let sample_rate = f64::from(info.sample_rate);

        let clap_node = if source == Source::Clap {
            instrument.and_then(|plugin| clap_instrument_node(plugin, sample_rate, info.max_block))
        } else {
            None
        };
        let (source, node) = match (source, clap_node) {
            (Source::Clap, Some(node)) => (Source::Clap, node),
            (Source::Clap, None) => {
                log::warn!(
                    "no usable CLAP instrument; starting on '{}' instead",
                    Source::DEFAULT.id()
                );
                (Source::DEFAULT, self.source_node(Source::DEFAULT, &info)?)
            }
            (source, _) => (source, self.source_node(source, &info)?),
        };

        let effect_node =
            effect.and_then(|plugin| clap_effect_node(plugin, sample_rate, info.max_block));
        install_engine_graph(&self.audio, &self.source, source, node, effect_node)?;
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

    /// Build the node for a source that does not need the main thread.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] is impossible here (the caller holds a
    /// [`StreamInfo`]); a missing built-in instrument is a wiring bug, so it is
    /// logged and the test tone stands in rather than failing the whole start.
    fn source_node(
        &self,
        source: Source,
        info: &StreamInfo,
    ) -> Result<Box<dyn RtNode>, AudioIoError> {
        let sample_rate = f64::from(info.sample_rate);
        match self.build_source_node(source, sample_rate, info.max_block) {
            Ok(node) => Ok(node),
            Err(err) => {
                log::warn!("{err}; falling back to the test tone");
                Ok(self.tone.create(sample_rate, info.max_block))
            }
        }
    }

    /// Build the RT node for an internal source.
    ///
    /// # Errors
    /// [`SourceError::ClapNotConfigured`] for [`Source::Clap`], which is not
    /// buildable from here at all (it needs the main thread), and
    /// [`SourceError::MissingInstrument`] when the instruments plugin did not
    /// contribute the node -- a wiring bug rather than a runtime condition.
    fn build_source_node(
        &self,
        source: Source,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Box<dyn RtNode>, SourceError> {
        match source {
            Source::TestTone => Ok(self.tone.create(sample_rate, max_block)),
            Source::Clap => Err(SourceError::ClapNotConfigured),
            other => {
                let type_id = other
                    .instrument_type_id()
                    .ok_or(SourceError::ClapNotConfigured)?;
                self.instruments
                    .create(type_id, sample_rate, max_block)
                    .ok_or(SourceError::MissingInstrument(type_id))
            }
        }
    }

    /// Change what plays at the head of the chain, while the engine runs.
    ///
    /// Internal sources are built and pushed straight from the calling thread
    /// -- a factory is `Send + Sync` and the swap is a wait-free queue push.
    /// [`Source::Clap`] is the exception: only the main thread can activate a
    /// hosted plugin, so it goes through [`ClapSourceBridge`] and waits.
    ///
    /// # Errors
    /// [`SourceError::Audio`] when no device is open, [`SourceError::QueueFull`]
    /// when the audio thread is behind (retryable),
    /// [`SourceError::ClapNotConfigured`] when `clap` was asked for on a session
    /// with no bundle loaded, and [`SourceError::MainThread`] for whatever the
    /// run loop reported.
    pub fn set_source(&self, source: Source) -> Result<(), SourceError> {
        if source == Source::Clap {
            return self.clap_source.request();
        }
        let info = self.audio.stream_info().ok_or(AudioIoError::NotRunning)?;
        let node = self.build_source_node(source, f64::from(info.sample_rate), info.max_block)?;
        self.source.install(node, source)?;
        log::info!("source is now '{}'", source.id());
        Ok(())
    }

    /// Change the source from its string id, as the webview sends it.
    ///
    /// # Errors
    /// [`SourceError::Unknown`] for an id that names nothing, plus everything
    /// [`set_source`](Self::set_source) can report.
    pub fn set_source_id(&self, id: &str) -> Result<(), SourceError> {
        self.set_source(Source::parse(id)?)
    }

    /// The source currently at the head of the chain.
    pub fn current_source(&self) -> Source {
        self.source.current()
    }

    /// Every source, marked with which is playing and which can be chosen.
    pub fn sources(&self) -> Vec<SourceDto> {
        let current = self.current_source();
        let clap = self.clap_source.is_available();
        Source::ALL
            .into_iter()
            .map(|source| SourceDto::new(source, current, clap))
            .collect()
    }

    /// The head-of-chain handle, for the main thread's CLAP source path.
    pub fn source_handle(&self) -> &Arc<SourceHandle> {
        &self.source
    }

    /// The CLAP source mailbox. Commands post here; the run loop serves it.
    pub fn clap_source(&self) -> &Arc<ClapSourceBridge> {
        &self.clap_source
    }

    /// Open the output device and publish `test-tone -> [effect] -> gate`.
    ///
    /// # Errors
    /// See [`start_audio_with_plugins`](Self::start_audio_with_plugins).
    pub fn start_audio_with_effect(
        &self,
        effect: Option<&mut LoadedClapPlugin>,
    ) -> Result<StreamInfo, AudioIoError> {
        self.start_audio_with_plugins(None, effect)
    }

    /// Open the output device on the source implied by the plugins given.
    ///
    /// With an `instrument`, [`Source::Clap`]; without one, [`Source::TestTone`]
    /// -- which is what the hardware tests and the headless capture path want,
    /// and deliberately *not* the shell's startup default (see
    /// [`Source::DEFAULT`]). To start on anything else, call
    /// [`start_audio_with_source`](Self::start_audio_with_source).
    ///
    /// The caller keeps ownership of each [`LoadedClapPlugin`] (they are `!Send`
    /// and must outlive the nodes they create), which is why this takes `&mut`
    /// rather than storing them: `AppState` lives in Tauri's managed state and
    /// must stay `Send + Sync`.
    ///
    /// # Errors
    /// Whatever cpal reports, plus [`AudioIoError::NotRunning`] if the stream died
    /// between opening it and installing the graph.
    pub fn start_audio_with_plugins(
        &self,
        instrument: Option<&mut LoadedClapPlugin>,
        effect: Option<&mut LoadedClapPlugin>,
    ) -> Result<StreamInfo, AudioIoError> {
        let source = if instrument.is_some() {
            Source::Clap
        } else {
            Source::TestTone
        };
        self.start_audio_with_source(source, instrument, effect)
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

    /// Play a note, live.
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
        self.audio.send_note(LiveNote::note_on(key, velocity))?;
        Ok(())
    }

    /// Release a note.
    ///
    /// # Errors
    /// [`NoteError::KeyOutOfRange`], or [`NoteError::Audio`] when no device is
    /// open.
    pub fn note_off(&self, key: u8) -> Result<(), NoteError> {
        if key > MAX_KEY {
            return Err(NoteError::KeyOutOfRange(key));
        }
        self.audio.send_note(LiveNote::note_off(key))?;
        Ok(())
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

    /// The plugin-GUI mailbox. Commands post here; the run loop serves it.
    pub fn gui(&self) -> &Arc<ClapGuiBridge> {
        &self.gui
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
            vec!["kernel-status", "meters", "step-sequencer", "clap-browser"],
            "panels appear in kernel boot order after the shell's own; \
             debug-tap deliberately contributes none"
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
    fn the_source_list_offers_every_source_and_starts_on_the_default() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();

        assert_eq!(state.current_source(), Source::DEFAULT);
        let sources = state.sources();
        assert_eq!(
            sources.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["poly-synth", "drum-kit", "test-tone", "clap"]
        );
        let selected: Vec<&str> = sources
            .iter()
            .filter(|s| s.selected)
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(selected, vec!["poly-synth"], "exactly one is playing");

        // No `MYDAW_CLAP_INSTRUMENT` in the test process, so the CLAP source is
        // listed but not offerable -- the UI greys it out rather than hiding it,
        // because "you could have one" is useful information.
        let clap = sources.iter().find(|s| s.id == "clap").unwrap();
        assert!(!clap.available);
        assert!(sources
            .iter()
            .filter(|s| s.id != "clap")
            .all(|s| s.available));
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
            Source::DEFAULT,
            "a failed switch must not change what the UI is told is playing"
        );
    }

    #[test]
    fn the_clap_source_is_refused_immediately_when_no_bundle_was_loaded() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();

        // Not a five-second timeout waiting for a main thread that is not
        // there: the shell publishes availability at startup, and this session
        // has none.
        let err = state.set_source_id("clap").unwrap_err();
        assert!(matches!(err, SourceError::ClapNotConfigured), "{err:?}");
        assert!(err.to_string().contains("MYDAW_CLAP_INSTRUMENT"), "{err}");
        assert_eq!(state.clap_source().pending(), 0);
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
        for source in [Source::TestTone, Source::PolySynth, Source::DrumKit] {
            assert!(
                state.build_source_node(source, 48_000.0, 512).is_ok(),
                "'{}' could not be built",
                source.id()
            );
        }
        assert!(state
            .build_source_node(Source::Clap, 48_000.0, 512)
            .is_err());
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
