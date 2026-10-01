//! Loading a CLAP plugin, activating it, and running it inside the RT graph.
//!
//! This is Phase 1 spike 2. [`super::ClapHostService::scan`] finds plugins;
//! this module makes one of them *process audio*.
//!
//! # Thread model (CLAP's, not ours)
//!
//! CLAP splits a plugin instance across three thread specifications, and getting
//! them wrong is undefined behaviour rather than a bug you can debug. `clack`
//! encodes them in the type system, and so does this module:
//!
//! ```text
//!  main thread                                  audio thread
//!  -----------                                  ------------
//!  PluginEntry::load           (!Send)
//!  PluginInstance::new         (!Send) ---------------------------.
//!  instance.activate()  -> StoppedPluginAudioProcessor (Send)     |
//!                                     |                           |
//!                                     '--- moved into ClapNode ---'
//!                                          start_processing()
//!                                          process() x N
//!  instance.deactivate(stopped) <-- ClapNode::drop sends it back
//! ```
//!
//! [`LoadedClapPlugin`] is deliberately **not** `Send`: it owns the
//! `PluginInstance`, which CLAP pins to the main thread. Only [`ClapNode`] --
//! which owns nothing but the audio processor, buffers and a return channel --
//! crosses the wall.
//!
//! # The teardown problem, and what we do about it
//!
//! MYDAW retires audio graphs through `basedrop`: the audio thread drops an
//! `Owned<CompiledGraph>`, and the *collector thread* runs the destructors later.
//! That is exactly right for buffers and exactly wrong for a CLAP plugin:
//! `clap_plugin->deactivate()` is a `[main-thread]` call, and the collector
//! thread is not the main thread.
//!
//! So [`ClapNode`] does not deactivate anything when it is dropped. It sends its
//! [`StartedPluginAudioProcessor`] back through a channel to the
//! [`LoadedClapPlugin`] that created it (the processor is `Send`; the instance is
//! not). Whoever owns the `LoadedClapPlugin` -- the main thread -- calls
//! [`LoadedClapPlugin::collect_retired`] (or lets `Drop` do it) and *there*
//! performs `stop_processing()` + `deactivate()`, on the correct thread.
//!
//! The one ordering rule that follows: **the `LoadedClapPlugin` must outlive every
//! `ClapNode` it created.** If it does not, `clack` deliberately leaks the plugin
//! instance rather than destroying it from the wrong thread — safe, but a leak.
//! [`LoadedClapPlugin::shutdown`] exists so shutdown code can wait for the node
//! to come back instead of racing the collector.

use std::ffi::{CString, NulError};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

use clack_extensions::audio_ports::PluginAudioPorts;
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::note_ports::PluginNotePorts;
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::state::{HostState, HostStateImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent, ParamValueEvent};
use clack_host::events::io::EventBuffer;
use clack_host::events::{Match, Pckn};
use clack_host::prelude::*;
use mydaw_rt_core::{NoteKind, ProcessCtx, RtNode, CHANNELS, DEFAULT_NOTE_CAPACITY};

use crate::timers::ClapTimers;
use crate::ClapPluginInfo;

/// Host identity handed to every plugin we instantiate.
const HOST_NAME: &str = "MYDAW";
const HOST_VENDOR: &str = "MYDAW";
const HOST_URL: &str = "https://github.com/mydaw";
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long [`LoadedClapPlugin::shutdown`] waits for a retired node to come back.
///
/// The node is normally dropped by the `basedrop` collector thread, which sweeps
/// every 250 ms, so this is roughly eight sweeps of slack. Waiting is only ever
/// done on a shutdown path, never while audio is running.
pub const RETIRE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long [`LoadedClapPlugin::wait_for_retire`] blocks on one receive before
/// re-checking the deadline.
///
/// The receive is woken by the collector, so this only bounds how long a
/// *timed-out* wait overshoots by; it is not a polling interval.
const RETIRE_POLL: Duration = Duration::from_millis(25);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong between "here is a path" and "audio is flowing".
///
/// Every variant is reachable from plugin-controlled data: a bundle that will not
/// load, a descriptor that lies, an `activate()` that returns false, a port layout
/// we cannot drive. None of them panics -- a hostile or simply broken plugin must
/// produce an `Err`, because a panic across an FFI boundary is undefined
/// behaviour, and a panic in the DAW is a lost session.
#[derive(Debug, thiserror::Error)]
pub enum ClapHostError {
    /// The OS loader or `clap_entry->init()` refused the bundle.
    #[error("could not load CLAP bundle {path}: {message}")]
    BundleLoad { path: PathBuf, message: String },

    /// The bundle loaded but exports no `clap_plugin_factory`.
    #[error("CLAP bundle {path} exposes no plugin factory")]
    NoPluginFactory { path: PathBuf },

    /// The bundle loaded, but no plugin in it has the requested id.
    #[error("CLAP bundle {path} contains no plugin with id '{id}'")]
    PluginNotFound { path: PathBuf, id: String },

    /// A plugin id is passed to C as a NUL-terminated string; one containing an
    /// interior NUL cannot be. Reachable from a project file or a UI text field.
    #[error("plugin id '{id}' contains an interior NUL byte")]
    InvalidPluginId { id: String, source: NulError },

    /// `clap_plugin_factory->create_plugin()` or `clap_plugin->init()` failed.
    #[error("could not instantiate CLAP plugin '{id}': {message}")]
    Instantiate { id: String, message: String },

    /// The audio configuration is not one any plugin can be activated with.
    /// Checked by us: `clack` *panics* on a zero block size, and a panic is not
    /// an acceptable answer to a device reporting something silly.
    #[error("invalid audio configuration: {message}")]
    InvalidAudioConfiguration { message: String },

    /// `clap_plugin->activate()` failed, or the instance was already active.
    #[error("could not activate CLAP plugin: {message}")]
    Activate { message: String },

    /// `clap_plugin->start_processing()` failed.
    #[error("could not start processing: {message}")]
    StartProcessing { message: String },

    /// The plugin's ports are not something the Phase 1 engine can drive.
    #[error("unsupported audio port layout: {message}")]
    UnsupportedPortLayout { message: String },

    /// Plugin code panicked through the FFI boundary. Caught, reported, and the
    /// plugin is not used.
    #[error("CLAP plugin panicked while {stage}")]
    Panicked { stage: &'static str },

    /// No scanned plugin has this id.
    #[error("no scanned CLAP plugin has id '{0}'")]
    UnknownPluginId(String),

    /// The host's own identity strings could not be converted for C. A bug here,
    /// never plugin-controlled -- kept typed so no code path unwraps.
    #[error("host information is not representable as C strings: {0}")]
    HostInfo(#[source] NulError),
}

// ---------------------------------------------------------------------------
// Host callback handlers
// ---------------------------------------------------------------------------

/// Requests the plugin has made of the host, latched as atomics.
///
/// CLAP's `request_*` callbacks are `[thread-safe]`: a plugin may call them from
/// its audio thread, so the implementations must be RT-safe. Atomics only -- no
/// locking, no allocation, no logging. The main thread reads and clears them.
#[derive(Debug, Default)]
pub struct ClapHostRequests {
    pub(crate) parameters: OnceLock<Arc<mydaw_plugin_params::ParameterBank>>,
    restart: AtomicBool,
    process: AtomicBool,
    callback: AtomicBool,
}

impl ClapHostRequests {
    pub fn parameters_bank(&self) -> &mydaw_plugin_params::ParameterBank {
        self.parameters
            .get()
            .expect("initialized before plugin creation")
    }
    /// Take the "please deactivate and reactivate me" flag, clearing it.
    pub fn take_restart(&self) -> bool {
        self.restart.swap(false, Ordering::AcqRel)
    }

    /// Take the "please resume calling process()" flag, clearing it.
    pub fn take_process(&self) -> bool {
        self.process.swap(false, Ordering::AcqRel)
    }

    /// Take the "please call on_main_thread()" flag, clearing it.
    pub fn take_callback(&self) -> bool {
        self.callback.swap(false, Ordering::AcqRel)
    }
}

/// What a plugin's *editor* has asked the host for, latched as atomics.
///
/// CLAP's `clap_host_gui` callbacks are `[thread-safe]`: `request_resize` in
/// particular may arrive from a plugin's own worker thread. Acting on one means
/// touching a window, which is main-thread-only work, so the callbacks record and
/// the main thread acts -- the same shape as [`ClapHostRequests`].
#[derive(Debug, Default)]
pub struct ClapGuiRequests {
    /// Packed [`GuiSize`], valid only while `resize_pending` is set.
    resize: AtomicU64,
    resize_pending: AtomicBool,
    show: AtomicBool,
    hide: AtomicBool,
    hints_changed: AtomicBool,
    closed: AtomicBool,
    /// Set when the plugin destroyed its own GUI before telling us
    /// (`closed(was_destroyed = true)`): the host must then not call `destroy`.
    self_destroyed: AtomicBool,
}

impl ClapGuiRequests {
    /// Take the size the plugin asked its window to become, if any.
    pub fn take_resize(&self) -> Option<GuiSize> {
        if self.resize_pending.swap(false, Ordering::AcqRel) {
            Some(GuiSize::unpack_from_u64(
                self.resize.load(Ordering::Acquire),
            ))
        } else {
            None
        }
    }

    /// Take the "please show my editor" flag.
    pub fn take_show(&self) -> bool {
        self.show.swap(false, Ordering::AcqRel)
    }

    /// Take the "please hide my editor" flag.
    pub fn take_hide(&self) -> bool {
        self.hide.swap(false, Ordering::AcqRel)
    }

    /// Take the "my resize hints changed" flag.
    pub fn take_hints_changed(&self) -> bool {
        self.hints_changed.swap(false, Ordering::AcqRel)
    }

    /// Take the "my editor is gone" flag.
    pub fn take_closed(&self) -> bool {
        self.closed.swap(false, Ordering::AcqRel)
    }

    /// Whether the plugin destroyed its own GUI. Sticky until the host tears the
    /// session down, because calling `destroy` on an already-destroyed GUI is
    /// exactly the kind of double-free CLAP's `was_destroyed` flag exists to
    /// prevent.
    pub fn was_self_destroyed(&self) -> bool {
        self.self_destroyed.load(Ordering::Acquire)
    }

    /// Forget everything. Called when a GUI session ends, so a stale request
    /// cannot be applied to the next one.
    pub fn clear(&self) {
        self.resize_pending.store(false, Ordering::Release);
        self.show.store(false, Ordering::Release);
        self.hide.store(false, Ordering::Release);
        self.hints_changed.store(false, Ordering::Release);
        self.closed.store(false, Ordering::Release);
        self.self_destroyed.store(false, Ordering::Release);
    }
}

/// `[thread-safe]` half of the host. Shared with the plugin for its whole life.
pub struct ClapHostShared {
    requests: Arc<ClapHostRequests>,
    gui: Arc<ClapGuiRequests>,
}

impl HostGuiImpl for ClapHostShared {
    fn resize_hints_changed(&self) {
        self.gui.hints_changed.store(true, Ordering::Release);
    }

    /// Latch the request; the main thread resizes the window on its next pass.
    ///
    /// CLAP explicitly allows an asynchronous answer here: a successful return
    /// means "acknowledged", and if the size turns out to be impossible the host
    /// tells the plugin by calling `set_size` with what it actually got.
    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        self.gui
            .resize
            .store(new_size.pack_to_u64(), Ordering::Release);
        self.gui.resize_pending.store(true, Ordering::Release);
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        self.gui.show.store(true, Ordering::Release);
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        self.gui.hide.store(true, Ordering::Release);
        Ok(())
    }

    fn closed(&self, was_destroyed: bool) {
        if was_destroyed {
            self.gui.self_destroyed.store(true, Ordering::Release);
        }
        self.gui.closed.store(true, Ordering::Release);
    }
}

impl SharedHandler<'_> for ClapHostShared {
    fn request_restart(&self) {
        // Possibly called from the audio thread: one relaxed store, nothing else.
        self.requests.restart.store(true, Ordering::Release);
    }

    fn request_process(&self) {
        self.requests.process.store(true, Ordering::Release);
    }

    fn request_callback(&self) {
        self.requests.callback.store(true, Ordering::Release);
    }
}

/// `[main-thread]` half of the host.
///
/// Owns the plugin's timer registrations. `clap.timer-support` is a main-thread
/// extension in both directions: the plugin registers from the main thread and
/// the host must call `on_timer` there too.
pub struct ClapHostMainThread {
    timers: Arc<ClapTimers>,
    /// Latched `clap_host_state->mark_dirty`. See [`crate::state`].
    state_dirty: Arc<AtomicBool>,
}

impl HostParamsImplShared for ClapHostShared {
    fn request_flush(&self) {
        self.requests.process.store(true, Ordering::Release);
    }
}
impl HostParamsImplMainThread for ClapHostMainThread {
    fn rescan(&mut self, _flags: ParamRescanFlags) {
        self.state_dirty.store(true, Ordering::Release);
    }
    fn clear(&mut self, _id: ClapId, _flags: ParamClearFlags) {
        self.state_dirty.store(true, Ordering::Release);
    }
}

impl MainThreadHandler<'_> for ClapHostMainThread {}

impl HostStateImpl for ClapHostMainThread {
    /// The plugin's state has changed since the host last saved or loaded it --
    /// a knob turned in its editor, say.
    ///
    /// Recorded rather than acted on: step 6 has no project file to mark
    /// modified, and a host that saved on every knob movement would write a
    /// megabyte per gesture. [`LoadedClapPlugin::take_state_dirty`] is what
    /// step 8 will read.
    fn mark_dirty(&mut self) {
        self.state_dirty.store(true, Ordering::Release);
    }
}

impl HostTimerImpl for ClapHostMainThread {
    /// Register a timer. The period is clamped to something this host can
    /// actually deliver -- CLAP allows the host to adjust a period that is too
    /// short, and promising 1 ms when the message loop runs at ~60 Hz would be a
    /// lie the plugin cannot detect.
    fn register_timer(&mut self, period_ms: u32) -> Result<TimerId, HostError> {
        Ok(self.timers.register(period_ms))
    }

    fn unregister_timer(&mut self, timer_id: TimerId) -> Result<(), HostError> {
        if self.timers.unregister(timer_id) {
            Ok(())
        } else {
            Err(HostError::Message("no such timer"))
        }
    }
}

/// Ties the three handler types together. See [`HostHandlers`].
pub struct MydawClapHost;

impl HostHandlers for MydawClapHost {
    type Shared<'a> = ClapHostShared;
    type MainThread<'a> = ClapHostMainThread;
    /// No audio-thread callbacks are implemented yet, so the unit type does.
    type AudioProcessor<'a> = ();

    /// The host extensions MYDAW implements.
    ///
    /// `gui` and `timer-support` are what a plugin editor needs from its host:
    /// somewhere to ask for a different window size, and a heartbeat to animate
    /// on. Both are serviced on the main thread by
    /// [`LoadedClapPlugin::tick_main_thread`].
    ///
    /// `state` is declared so a plugin can tell the host its settings changed
    /// (`mark_dirty`). The *reading* half of `clap.state` is a plugin extension
    /// and needs no declaration; see [`crate::state`].
    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &ClapHostShared) {
        builder.register::<HostGui>();
        builder.register::<HostTimer>();
        builder.register::<HostState>();
        builder.register::<HostParams>();
    }
}

/// What one [`LoadedClapPlugin::tick_main_thread`] pass actually did.
///
/// Returned rather than logged: at 60 Hz a log line per tick would be noise, and
/// a test wants a number.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MainThreadWork {
    /// `clap_plugin_timer_support->on_timer` calls made.
    pub timers_fired: usize,
    /// Whether `clap_plugin->on_main_thread` was called.
    pub callback: bool,
    /// Whether the plugin asked to be deactivated and reactivated. Recorded, not
    /// acted on: rebuilding the graph is the shell's business (see the spike 3
    /// spec's limitations).
    pub restart_requested: bool,
}

impl MainThreadWork {
    /// Whether anything happened at all.
    pub const fn is_idle(&self) -> bool {
        self.timers_fired == 0 && !self.callback && !self.restart_requested
    }
}

// ---------------------------------------------------------------------------
// Port layout
// ---------------------------------------------------------------------------

/// What a plugin says about its ports.
///
/// Read once, on the main thread, before activation: CLAP only allows a plugin to
/// change its port configuration while deactivated, so a layout read here stays
/// valid for the life of the activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClapPortLayout {
    pub input_ports: u32,
    pub output_ports: u32,
    /// Channel count of input port 0.
    pub main_input_channels: u32,
    /// Channel count of output port 0.
    pub main_output_channels: u32,
    /// Note input ports. Zero for a plugin with no `note-ports` extension --
    /// per the CLAP specification that means *no* note ports, so the host must
    /// not send it events.
    pub note_input_ports: u32,
    /// Note output ports. Recorded for completeness; MYDAW does not read a
    /// plugin's note output yet (see [`ClapNode::process`]).
    pub note_output_ports: u32,
}

impl ClapPortLayout {
    /// Whether this layout is the stereo-in/stereo-out shape of an insert
    /// effect. Anything wider is a Phase 2 mixer feature.
    pub fn is_stereo_effect(&self) -> bool {
        self.input_ports >= 1
            && self.output_ports >= 1
            && self.main_input_channels == CHANNELS as u32
            && self.main_output_channels == CHANNELS as u32
    }

    /// Whether this layout is an instrument: no audio in, stereo out.
    ///
    /// A note input is not required. A plugin that generates without being
    /// played (a noise source, a test oscillator) is still an instrument as far
    /// as the graph is concerned; it just never receives events.
    pub fn is_instrument(&self) -> bool {
        self.input_ports == 0
            && self.output_ports >= 1
            && self.main_output_channels == CHANNELS as u32
    }

    /// Whether the Phase 1 graph can drive this plugin at all.
    pub fn is_supported(&self) -> bool {
        self.is_stereo_effect() || self.is_instrument()
    }

    /// Whether the plugin takes audio input, and therefore whether
    /// [`ClapNode`] hands it the bus.
    pub fn has_audio_input(&self) -> bool {
        self.input_ports >= 1
    }

    /// Whether the plugin accepts note events on port 0.
    pub fn has_note_input(&self) -> bool {
        self.note_input_ports >= 1
    }
}

// ---------------------------------------------------------------------------
// Loaded plugin
// ---------------------------------------------------------------------------

/// A CLAP plugin instance living on the main thread.
///
/// Not `Send`, by construction: it owns a `PluginInstance`, and CLAP pins
/// instance lifecycle calls (`init`, `activate`, `deactivate`, `destroy`) to the
/// thread that created it.
///
/// Create nodes with [`create_node`](Self::create_node); retire them by dropping
/// the node and then calling [`collect_retired`](Self::collect_retired) or
/// [`shutdown`](Self::shutdown) here. See the module docs for why.
pub struct LoadedClapPlugin {
    /// Kept alive explicitly. `clack` also stores a clone inside the instance --
    /// unloading the library out from under a live plugin would be fatal.
    _entry: PluginEntry,
    pub(crate) instance: PluginInstance<MydawClapHost>,
    info: ClapPluginInfo,
    layout: ClapPortLayout,
    pub(crate) requests: Arc<ClapHostRequests>,
    gui_requests: Arc<ClapGuiRequests>,
    timers: Arc<ClapTimers>,
    /// Latched `clap_host_state->mark_dirty`. See [`crate::state`].
    state_dirty: Arc<AtomicBool>,
    /// Reused by [`tick_main_thread`](Self::tick_main_thread) so servicing timers
    /// allocates nothing per frame.
    due_timers: Vec<TimerId>,
    /// Given to every node so it can hand its processor back on drop.
    retire_tx: Sender<StartedPluginAudioProcessor<MydawClapHost>>,
    retire_rx: Receiver<StartedPluginAudioProcessor<MydawClapHost>>,
    /// True between `create_node` and a successful retire.
    node_outstanding: bool,
}

impl LoadedClapPlugin {
    /// Load `path` and instantiate the plugin with CLAP id `plugin_id`.
    ///
    /// Runs third-party code twice over: once for the bundle entry, once for
    /// `create_plugin` + `init`. Both are wrapped in [`catch_unwind`], so a plugin
    /// that panics through FFI produces [`ClapHostError::Panicked`] instead of
    /// unwinding into the caller. A hard crash (segfault, `abort`) is still fatal
    /// -- see the crate docs on out-of-process scanning.
    ///
    /// # Errors
    /// Any [`ClapHostError`]; none of them are fatal to the caller.
    pub fn load(path: &Path, plugin_id: &str) -> Result<Self, ClapHostError> {
        let id = CString::new(plugin_id).map_err(|source| ClapHostError::InvalidPluginId {
            id: plugin_id.to_owned(),
            source,
        })?;
        let host_info = HostInfo::new(HOST_NAME, HOST_VENDOR, HOST_URL, HOST_VERSION)
            .map_err(ClapHostError::HostInfo)?;

        // SAFETY (inside): `PluginEntry::load` runs the DLL entry point and
        // `clap_entry->init()`. Unavoidable when hosting; `clack` upholds the
        // entry protocol for the value it returns.
        let loaded = catch_unwind(AssertUnwindSafe(|| -> Result<_, ClapHostError> {
            let entry =
                unsafe { PluginEntry::load(path) }.map_err(|err| ClapHostError::BundleLoad {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })?;

            let info = {
                let factory =
                    entry
                        .get_plugin_factory()
                        .ok_or_else(|| ClapHostError::NoPluginFactory {
                            path: path.to_path_buf(),
                        })?;
                factory
                    .plugin_descriptors()
                    .filter_map(|descriptor| ClapPluginInfo::from_descriptor(descriptor, path))
                    .find(|candidate| candidate.id == plugin_id)
                    .ok_or_else(|| ClapHostError::PluginNotFound {
                        path: path.to_path_buf(),
                        id: plugin_id.to_owned(),
                    })?
            };

            let requests = Arc::new(ClapHostRequests::default());
            let bank = mydaw_plugin_params::ParameterBank::new(vec![])
                .map_err(|message| ClapHostError::InvalidAudioConfiguration { message })?;
            let _ = requests.parameters.set(Arc::new(bank));
            let gui_requests = Arc::new(ClapGuiRequests::default());
            let timers = Arc::new(ClapTimers::new());
            let state_dirty = Arc::new(AtomicBool::new(false));
            let shared_requests = Arc::clone(&requests);
            let shared_gui = Arc::clone(&gui_requests);
            let main_thread_timers = Arc::clone(&timers);
            let main_thread_dirty = Arc::clone(&state_dirty);
            let instance = PluginInstance::<MydawClapHost>::new(
                move |_| ClapHostShared {
                    requests: shared_requests,
                    gui: shared_gui,
                },
                move |_| ClapHostMainThread {
                    timers: main_thread_timers,
                    state_dirty: main_thread_dirty,
                },
                &entry,
                &id,
                &host_info,
            )
            .map_err(|err| ClapHostError::Instantiate {
                id: plugin_id.to_owned(),
                message: err.to_string(),
            })?;

            Ok((
                entry,
                instance,
                info,
                requests,
                gui_requests,
                timers,
                state_dirty,
            ))
        }))
        .map_err(|_| ClapHostError::Panicked { stage: "loading" })??;

        let (entry, mut instance, info, requests, gui_requests, timers, state_dirty) = loaded;

        let layout = read_port_layout(&mut instance)?;
        let (retire_tx, retire_rx) = mpsc::channel();

        Ok(Self {
            _entry: entry,
            instance,
            info,
            layout,
            requests,
            gui_requests,
            timers,
            state_dirty,
            due_timers: Vec::new(),
            retire_tx,
            retire_rx,
            node_outstanding: false,
        })
    }

    /// Identity of the plugin that was actually instantiated.
    pub fn info(&self) -> &ClapPluginInfo {
        &self.info
    }

    /// Port layout as reported by the plugin before activation.
    pub fn port_layout(&self) -> ClapPortLayout {
        self.layout
    }

    /// Latched `request_restart` / `request_process` / `request_callback` flags.
    pub fn requests(&self) -> &Arc<ClapHostRequests> {
        &self.requests
    }

    /// Latched `clap_host_gui` callbacks. Serviced by the GUI session, if one is
    /// open; see [`crate::gui`].
    pub fn gui_requests(&self) -> &Arc<ClapGuiRequests> {
        &self.gui_requests
    }

    /// The plugin's registered `clap.timer-support` timers.
    pub fn timers(&self) -> &Arc<ClapTimers> {
        &self.timers
    }

    /// Latched `clap_host_state->mark_dirty`. See [`crate::state`].
    pub fn state_dirty(&self) -> &Arc<AtomicBool> {
        &self.state_dirty
    }

    /// Whether a node created from this plugin is still outstanding.
    ///
    /// True between [`create_node`](Self::create_node) and the moment the
    /// dropped node's processor comes back through the retire channel -- which
    /// happens on the `basedrop` collector's schedule, not the caller's.
    pub const fn has_outstanding_node(&self) -> bool {
        self.node_outstanding
    }

    /// Wait (up to `timeout`) for a retired node's processor to come home, so
    /// the instance can be activated again.
    ///
    /// A CLAP plugin can only be activated once at a time, and MYDAW retires
    /// nodes through `basedrop`: the audio thread drops the node, and the
    /// *collector thread* runs its destructor some sweeps later. Re-activating
    /// -- which is what a graph rebuild and a source swap both do -- therefore
    /// has to wait for that, and this is the wait, in one place rather than
    /// copied into each caller.
    ///
    /// Returns `true` when the instance is inactive (i.e. ready to activate).
    /// Cheap and immediate when nothing was outstanding.
    pub fn wait_for_retire(&mut self, timeout: Duration) -> bool {
        self.collect_retired();
        if !self.instance.is_active() {
            return true;
        }
        let deadline = Instant::now() + timeout;
        while self.instance.is_active() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            // A bounded receive rather than a sleep loop: the collector wakes us
            // the instant it drops the node, and a poll interval would add up to
            // that interval of silence to every source swap.
            match self.retire_rx.recv_timeout(remaining.min(RETIRE_POLL)) {
                Ok(started) => self.deactivate(started),
                Err(_) => continue,
            }
        }
        !self.instance.is_active()
    }

    /// A main-thread handle to the plugin, for extensions the host drives itself.
    ///
    /// Only valid on the thread that loaded the plugin, which is what CLAP means
    /// by `[main-thread]`; `LoadedClapPlugin` being `!Send` is what enforces it.
    pub fn plugin_handle(&mut self) -> PluginMainThreadHandle<'_> {
        self.instance.plugin_handle()
    }

    /// One pass of the host's main-thread obligations: fire due timers, and run
    /// the `on_main_thread` callback if the plugin asked for one.
    ///
    /// Call it from the application's event loop, at least as often as the
    /// shortest registered timer period (MYDAW calls it from Tauri's
    /// `MainEventsCleared`, woken ~60 Hz by the plugin window's own timer). It is
    /// cheap when nothing is due: one `Instant::now`, one uncontended lock.
    ///
    /// Not RT-safe and not meant to be: this is the main thread.
    pub fn tick_main_thread(&mut self) -> MainThreadWork {
        let mut work = MainThreadWork {
            restart_requested: self.requests.take_restart(),
            ..MainThreadWork::default()
        };

        if self.requests.take_callback() {
            // `on_main_thread` is how a plugin wakes its own main thread after a
            // worker thread had something to say. Ignoring it (spike 2's
            // behaviour) leaves plugins waiting forever.
            self.instance.call_on_main_thread_callback();
            work.callback = true;
        }

        // Collected first, dispatched after the lock is released: a plugin is
        // allowed to register or unregister timers from inside `on_timer`, and
        // doing that while we held the registry would deadlock.
        self.timers
            .collect_due(Instant::now(), &mut self.due_timers);
        if !self.due_timers.is_empty() {
            let due = std::mem::take(&mut self.due_timers);
            let mut handle = self.instance.plugin_handle();
            if let Some(timer) = handle.get_extension::<PluginTimer>() {
                for id in &due {
                    // Re-check: `on_timer` may have unregistered a later one in
                    // this same batch, and firing a dead timer is a bug a plugin
                    // is entitled to trip over.
                    if self.timers.contains(*id) {
                        timer.on_timer(&mut handle, *id);
                        work.timers_fired += 1;
                    }
                }
            }
            self.due_timers = due;
            self.due_timers.clear();
        }

        work
    }

    /// Whether the instance is currently activated.
    pub fn is_active(&self) -> bool {
        self.instance.is_active()
    }

    /// Activate the plugin and wrap its audio processor in an [`RtNode`].
    ///
    /// Everything the RT path will ever need is allocated here: clack's port
    /// descriptor tables and the two stereo scratch buffers. [`ClapNode::process`]
    /// allocates nothing.
    ///
    /// A plugin can only be activated once at a time, so a second call while a
    /// node is outstanding returns [`ClapHostError::Activate`]. Retire the first
    /// node ([`collect_retired`](Self::collect_retired)) before asking for another.
    ///
    /// # Errors
    /// [`ClapHostError::InvalidAudioConfiguration`] for a nonsensical sample rate
    /// or block size, [`ClapHostError::UnsupportedPortLayout`] for anything that
    /// is not stereo in / stereo out, and [`ClapHostError::Activate`] /
    /// [`ClapHostError::StartProcessing`] for a plugin that refuses.
    pub fn create_node(
        &mut self,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<ClapNode, ClapHostError> {
        self.parameters()
            .map_err(|message| ClapHostError::InvalidAudioConfiguration { message })?;
        if std::env::var_os("MYDAW_RENDER_WORKER").is_some() {
            use clack_extensions::render::{PluginRender, RenderMode};
            let mut handle = self.instance.plugin_handle();
            if let Some(render) = handle.get_extension::<PluginRender>() {
                if render.has_realtime_requirement(&mut handle) {
                    return Err(ClapHostError::Activate {
                        message: "plugin requires realtime hardware; offline export is unavailable"
                            .into(),
                    });
                }
                render.set(&mut handle, RenderMode::Offline).map_err(|e| {
                    ClapHostError::Activate {
                        message: e.to_string(),
                    }
                })?;
            }
        }
        // Do this first: a node retired since the last call frees the instance.
        self.collect_retired();

        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(ClapHostError::InvalidAudioConfiguration {
                message: format!("sample rate {sample_rate} is not a positive, finite number"),
            });
        }
        if max_block == 0 {
            return Err(ClapHostError::InvalidAudioConfiguration {
                message: "maximum block size cannot be 0".to_owned(),
            });
        }
        let max_frames =
            u32::try_from(max_block).map_err(|_| ClapHostError::InvalidAudioConfiguration {
                message: format!("maximum block size {max_block} does not fit in u32"),
            })?;

        if !self.layout.is_supported() {
            return Err(ClapHostError::UnsupportedPortLayout {
                message: format!(
                    "'{}' reports {} input port(s) x {} ch and {} output port(s) x {} ch; \
                     the Phase 1 graph drives stereo in / stereo out (an effect) or \
                     no input / stereo out (an instrument)",
                    self.info.id,
                    self.layout.input_ports,
                    self.layout.main_input_channels,
                    self.layout.output_ports,
                    self.layout.main_output_channels,
                ),
            });
        }

        let configuration = PluginAudioConfiguration {
            sample_rate,
            // A host that never asks for fewer frames than 1 is telling the truth
            // and lets a plugin size its buffers to `max_frames`.
            min_frames_count: 1,
            max_frames_count: max_frames,
        };

        let instance = &mut self.instance;
        let stopped = catch_unwind(AssertUnwindSafe(|| {
            instance.activate(|_, _| (), configuration)
        }))
        .map_err(|_| ClapHostError::Panicked {
            stage: "activating",
        })?
        .map_err(|err| ClapHostError::Activate {
            message: err.to_string(),
        })?;

        let started = catch_unwind(AssertUnwindSafe(|| stopped.start_processing()))
            .map_err(|_| ClapHostError::Panicked {
                stage: "starting processing",
            })?
            .map_err(|err| {
                let message = err.to_string();
                // Hand the still-stopped processor back so the instance is
                // deactivated properly instead of being stranded active.
                self.instance.deactivate(err.into_stopped_processor());
                ClapHostError::StartProcessing { message }
            })?;

        self.node_outstanding = true;
        Ok(ClapNode::new(
            started,
            max_block,
            self.layout,
            self.retire_tx.clone(),
            Arc::clone(&self.requests),
        ))
    }

    /// Deactivate any processor a dropped [`ClapNode`] has handed back.
    ///
    /// Cheap and non-blocking: call it from the main thread whenever convenient.
    /// Returns the number of processors retired (0 or 1 in practice).
    pub fn collect_retired(&mut self) -> usize {
        let mut retired = 0;
        // `try_recv` returning `Disconnected` is impossible (this type owns a
        // sender), and both error cases mean the same thing here: nothing waiting.
        while let Ok(started) = self.retire_rx.try_recv() {
            self.deactivate(started);
            retired += 1;
        }
        retired
    }

    /// Wait (up to [`RETIRE_TIMEOUT`]) for an outstanding node to come back, then
    /// deactivate. Returns `true` if the instance ended up inactive.
    ///
    /// This is the shutdown path. The node is normally dropped by the `basedrop`
    /// collector thread some time after the graph is retired, so a shutdown that
    /// does not wait would race it and leak the plugin instance.
    pub fn shutdown(&mut self) -> bool {
        self.collect_retired();
        if self.node_outstanding && self.instance.is_active() {
            let deadline = Instant::now() + RETIRE_TIMEOUT;
            while self.instance.is_active() {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                match self.retire_rx.recv_timeout(remaining) {
                    Ok(started) => self.deactivate(started),
                    Err(_) => break,
                }
            }
        }

        if self.instance.is_active() {
            // `try_deactivate` still succeeds if the node was dropped without its
            // processor reaching us (a disconnected channel, say).
            match self.instance.try_deactivate() {
                Ok(()) => self.node_outstanding = false,
                Err(err) => log::warn!(
                    "CLAP plugin '{}' could not be deactivated ({err}); the instance will be \
                     leaked rather than destroyed off the main thread",
                    self.info.id
                ),
            }
        }
        !self.instance.is_active()
    }

    /// Stop + deactivate one returned processor, on this (main) thread.
    fn deactivate(&mut self, started: StartedPluginAudioProcessor<MydawClapHost>) {
        // `stop_processing` is an `[audio-thread]` call in CLAP's model, but the
        // audio thread is provably gone: it dropped the node, which is what sent
        // this processor here, and clack's type system guarantees no other handle
        // exists. This thread *is* the audio thread for this instance now.
        let stopped = started.stop_processing();
        self.instance.deactivate(stopped);
        self.node_outstanding = false;
    }
}

impl std::fmt::Debug for LoadedClapPlugin {
    /// Hand-written: `PluginInstance` is a raw FFI handle with no `Debug`, and the
    /// four fields below are the ones anyone debugging a hosting problem wants.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedClapPlugin")
            .field("id", &self.info.id)
            .field("path", &self.info.path)
            .field("layout", &self.layout)
            .field("active", &self.instance.is_active())
            .finish()
    }
}

impl Drop for LoadedClapPlugin {
    fn drop(&mut self) {
        // Last chance to deactivate on the right thread. `shutdown` logs if a node
        // is still out there; dropping anyway is the lesser evil (clack leaks the
        // instance instead of destroying it from a foreign thread).
        self.shutdown();
    }
}

/// Read the plugin's port layout through the `audio-ports` and `note-ports`
/// extensions.
///
/// A plugin that does not implement `audio-ports` has, per the CLAP
/// specification, *no* audio ports at all -- so refusing it is correct, not
/// pedantic: handing buffers to a plugin that declared none is a host bug. The
/// same rule applies to `note-ports`, but its absence is not an error: it simply
/// means the plugin takes no events, and the node silently skips event delivery.
fn read_port_layout(
    instance: &mut PluginInstance<MydawClapHost>,
) -> Result<ClapPortLayout, ClapHostError> {
    catch_unwind(AssertUnwindSafe(|| {
        let mut handle = instance.plugin_handle();
        let Some(ports) = handle.get_extension::<PluginAudioPorts>() else {
            return Err(ClapHostError::UnsupportedPortLayout {
                message: "plugin does not implement the 'audio-ports' extension, so it declares \
                          no audio ports"
                    .to_owned(),
            });
        };

        let input_ports = ports.count(&mut handle, true);
        let output_ports = ports.count(&mut handle, false);

        let mut buffer = clack_extensions::audio_ports::AudioPortInfoBuffer::new();
        let main_input_channels = if input_ports > 0 {
            ports
                .get(&mut handle, 0, true, &mut buffer)
                .map_or(0, |info| info.channel_count)
        } else {
            0
        };
        let main_output_channels = if output_ports > 0 {
            ports
                .get(&mut handle, 0, false, &mut buffer)
                .map_or(0, |info| info.channel_count)
        } else {
            0
        };

        // Absent extension -> no note ports. Not an error: most effects have
        // none, and the ones that do are the interesting case.
        let (note_input_ports, note_output_ports) = match handle.get_extension::<PluginNotePorts>()
        {
            Some(notes) => (
                notes.count(&mut handle, true),
                notes.count(&mut handle, false),
            ),
            None => (0, 0),
        };

        Ok(ClapPortLayout {
            input_ports,
            output_ports,
            main_input_channels,
            main_output_channels,
            note_input_ports,
            note_output_ports,
        })
    }))
    .map_err(|_| ClapHostError::Panicked {
        stage: "reporting its ports",
    })?
}

// ---------------------------------------------------------------------------
// The RT node
// ---------------------------------------------------------------------------

/// Counters a [`ClapNode`] maintains for non-RT observers.
///
/// Atomics rather than logging, for the same reason as everywhere else inside the
/// RT wall: `log::warn!` on the audio thread can allocate and take a lock.
#[derive(Debug, Default)]
pub struct ClapNodeStats {
    process_calls: AtomicU64,
    process_errors: AtomicU64,
    sleep_requests: AtomicU64,
    truncated_blocks: AtomicU64,
    events_sent: AtomicU64,
    events_dropped: AtomicU64,
}

impl ClapNodeStats {
    /// Blocks handed to the plugin since activation.
    pub fn process_calls(&self) -> u64 {
        self.process_calls.load(Ordering::Relaxed)
    }

    /// Blocks where `clap_plugin->process()` reported failure. The node passes
    /// audio through unchanged for those blocks (bypass), never garbage.
    pub fn process_errors(&self) -> u64 {
        self.process_errors.load(Ordering::Relaxed)
    }

    /// Blocks after which the plugin asked to be put to sleep. Counted only; the
    /// Phase 1 graph keeps calling `process()` regardless, which is always legal.
    pub fn sleep_requests(&self) -> u64 {
        self.sleep_requests.load(Ordering::Relaxed)
    }

    /// Blocks larger than the `max_block` the node was activated for. Non-zero
    /// means the tail of those blocks was silenced -- audible, and a sign the
    /// device's buffer grew past `AudioIoConfig::max_block`.
    pub fn truncated_blocks(&self) -> u64 {
        self.truncated_blocks.load(Ordering::Relaxed)
    }

    /// Note events handed to the plugin since activation.
    pub fn events_sent(&self) -> u64 {
        self.events_sent.load(Ordering::Relaxed)
    }

    /// Note events the node could not deliver: more events in one block than
    /// [`MAX_BLOCK_EVENTS`], or a [`NoteKind`] this host does not translate yet.
    ///
    /// Events for a plugin with no note input port are *not* counted here --
    /// that is a configuration, not a loss.
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped.load(Ordering::Relaxed)
    }
}

/// A CLAP plugin, running inside the audio graph.
///
/// # RT safety
/// [`process`](RtNode::process) allocates nothing. The scratch buffers and
/// clack's [`AudioPorts`] descriptor tables are built in
/// [`LoadedClapPlugin::create_node`]; per block the node only re-*points* those
/// tables at the same buffers (`AudioPorts::with_input_buffers` clears and refills
/// a `Vec` whose capacity was reserved up front), copies samples, and calls the
/// plugin. No locks, no syscalls, no logging.
///
/// # Drop
/// Dropping a node does **not** deactivate the plugin: it sends the audio
/// processor back to its [`LoadedClapPlugin`], which deactivates on the main
/// thread. See the module docs. Consequently a node may safely be dropped by the
/// `basedrop` collector thread, which is exactly what the graph does.
pub struct ClapNode {
    /// `None` only after drop has sent it home, or if the node was built bypassed.
    processor: Option<StartedPluginAudioProcessor<MydawClapHost>>,
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    /// Stereo scratch. CLAP wants distinct input and output buffers, and the graph
    /// bus is a single in-place buffer, so the node owns both sides.
    input: [Vec<f32>; CHANNELS],
    output: [Vec<f32>; CHANNELS],
    /// This block's note events in CLAP's own wire format. Pre-grown at
    /// activation; see [`warmed_event_buffer`].
    events_in: EventBuffer,
    layout: ClapPortLayout,
    max_block: usize,
    steady_time: u64,
    stats: Arc<ClapNodeStats>,
    requests: Arc<ClapHostRequests>,
    retire_tx: Sender<StartedPluginAudioProcessor<MydawClapHost>>,
}

impl ClapNode {
    fn new(
        processor: StartedPluginAudioProcessor<MydawClapHost>,
        max_block: usize,
        layout: ClapPortLayout,
        retire_tx: Sender<StartedPluginAudioProcessor<MydawClapHost>>,
        requests: Arc<ClapHostRequests>,
    ) -> Self {
        Self {
            processor: Some(processor),
            // One port, `CHANNELS` channels: reserving here is what makes the
            // per-block `with_*_buffers` calls allocation-free.
            input_ports: AudioPorts::with_capacity(CHANNELS, 1),
            output_ports: AudioPorts::with_capacity(CHANNELS, 1),
            input: [vec![0.0; max_block], vec![0.0; max_block]],
            output: [vec![0.0; max_block], vec![0.0; max_block]],
            events_in: warmed_event_buffer(MAX_BLOCK_EVENTS),
            layout,
            max_block,
            steady_time: 0,
            stats: Arc::new(ClapNodeStats::default()),
            requests,
            retire_tx,
        }
    }

    /// Counters for this node. Clone the `Arc` to watch it from a non-RT thread.
    pub fn stats(&self) -> &Arc<ClapNodeStats> {
        &self.stats
    }

    /// Requests the plugin has made of the host. Shared with the owning
    /// [`LoadedClapPlugin`].
    pub fn requests(&self) -> &Arc<ClapHostRequests> {
        &self.requests
    }

    /// Largest block this node was activated for.
    pub fn max_block(&self) -> usize {
        self.max_block
    }

    /// The port layout this node was built for.
    pub fn port_layout(&self) -> ClapPortLayout {
        self.layout
    }

    /// Whether this node forwards [`ProcessCtx::events`] to its plugin.
    pub fn delivers_notes(&self) -> bool {
        self.layout.has_note_input()
    }

    /// Translate this block's note events into CLAP's wire format.
    ///
    /// Returns how many were dropped. RT-safe: [`EventBuffer::clear`] keeps the
    /// allocation and every event pushed here is the same size as the ones the
    /// buffer was warmed with, so `push` only writes into existing capacity.
    fn fill_events(&mut self, events: &[mydaw_rt_core::NoteEvent], frames: usize) -> usize {
        self.events_in.clear();
        let bank = self
            .requests
            .parameters
            .get()
            .expect("initialized before activation");
        let mut changes = [(0, 0.0); mydaw_plugin_params::MAX_CHANGES_PER_BLOCK];
        let count = bank.drain_input(&mut changes);
        for &(id, value) in &changes[..count] {
            if let Some(id) = ClapId::from_raw(id) {
                self.events_in
                    .push(&crate::parameters::value_event(id, value));
            }
        }
        if !self.layout.has_note_input() || events.is_empty() {
            return 0;
        }

        // `frames >= 1` is guaranteed by the caller. CLAP requires every event
        // time to be inside the block; clamping is defensive, since the engine
        // already guarantees it.
        let last_frame = (frames - 1) as u32;
        let mut dropped = 0;
        for event in events {
            if self.events_in.len() as usize
                >= MAX_BLOCK_EVENTS + mydaw_plugin_params::MAX_CHANGES_PER_BLOCK
            {
                dropped += 1;
                continue;
            }
            let time = event.frame_offset.min(last_frame);
            match event.kind {
                NoteKind::NoteOn { key, velocity } => {
                    self.events_in.push(&NoteOnEvent::new(
                        time,
                        pckn(event.channel, key),
                        f64::from(velocity),
                    ));
                }
                NoteKind::NoteOff { key } => {
                    // Release velocity 0.0: MYDAW's note model has no release
                    // velocity yet, and 0.0 is what CLAP hosts send when they
                    // have nothing to say about it.
                    self.events_in
                        .push(&NoteOffEvent::new(time, pckn(event.channel, key), 0.0));
                }
                // A `NoteKind` added after this code was written. Counted, not
                // guessed at: silently reinterpreting an unknown event is how a
                // host makes a plugin sound wrong in a way nobody can trace.
                _ => dropped += 1,
            }
        }
        dropped
    }
}

/// Events a single block may carry into a plugin.
///
/// Matches the engine's own per-block note buffer, so anything the graph can
/// hold, the node can forward.
pub const MAX_BLOCK_EVENTS: usize = DEFAULT_NOTE_CAPACITY;

/// An [`EventBuffer`] with room for `capacity` note events, and no more.
///
/// `EventBuffer::with_capacity` reserves for the largest standard CLAP event
/// (a transport event, ~5x a note event), which for 1024 events is over a
/// megabyte per node. Filling and clearing reserves exactly what note events
/// need instead -- `clear` keeps the allocation -- at the cost of one loop on a
/// non-RT thread.
fn warmed_event_buffer(capacity: usize) -> EventBuffer {
    let mut buffer = EventBuffer::new();
    for _ in 0..capacity + mydaw_plugin_params::MAX_CHANGES_PER_BLOCK {
        buffer.push(&crate::parameters::value_event(
            ClapId::from_raw(0).expect("valid id"),
            0.0,
        ));
    }
    buffer.clear();
    buffer
}

/// Address one key on one channel of note port 0.
///
/// `note_id` is [`Match::All`]: MYDAW does not track per-note ids yet, so a
/// note-off has to match every voice the matching note-on started.
fn pckn(channel: u8, key: u8) -> Pckn {
    Pckn::new(0_u16, u16::from(channel), u16::from(key), Match::All)
}

impl RtNode for ClapNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames.min(self.max_block);
        if frames == 0 {
            return;
        }
        if ctx.frames > self.max_block {
            // Cannot grow the buffers here. Silence what we cannot process rather
            // than let stale samples through as a buzz.
            self.stats.truncated_blocks.fetch_add(1, Ordering::Relaxed);
            for channel in ctx.buffers.as_mut_slices() {
                channel[frames..].fill(0.0);
            }
        }

        // Notes first: the buffer has to be filled before `self` is split up for
        // the borrow dance below.
        let dropped = self.fill_events(ctx.events, frames);
        if dropped > 0 {
            self.stats
                .events_dropped
                .fetch_add(dropped as u64, Ordering::Relaxed);
        }
        let sent = u64::from(self.events_in.len());

        {
            let bus = ctx.buffers.as_slices();
            for (scratch, source) in self.input.iter_mut().zip(bus) {
                scratch[..frames].copy_from_slice(&source[..frames]);
            }
        }

        // Destructure so the port tables and the scratch buffers can be borrowed
        // at the same time; they are disjoint fields, but the compiler needs to
        // see that spelled out.
        let Self {
            processor,
            input_ports,
            output_ports,
            input,
            output,
            events_in,
            layout,
            steady_time,
            ..
        } = self;
        let Some(processor) = processor.as_mut() else {
            // Retired node still in a live graph: pass audio through untouched.
            return;
        };

        let (input_left, input_right) = input.split_at_mut(1);
        // An instrument declares no audio input port, so it must be handed
        // none. Passing it the bus "just in case" would contradict its own
        // `audio-ports` answer, which is a host bug even when a lenient plugin
        // tolerates it.
        let audio_in = if layout.has_audio_input() {
            input_ports.with_input_buffers([AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    [
                        InputChannel::variable(&mut input_left[0][..frames]),
                        InputChannel::variable(&mut input_right[0][..frames]),
                    ]
                    .into_iter(),
                ),
            }])
        } else {
            InputAudioBuffers::empty()
        };

        let (output_left, output_right) = output.split_at_mut(1);
        let mut audio_out = output_ports.with_output_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only(
                [
                    &mut output_left[0][..frames],
                    &mut output_right[0][..frames],
                ]
                .into_iter(),
            ),
        }]);

        // Output events are discarded. `OutputEvents::void()` cannot allocate,
        // whereas an `EventBuffer` output would grow without bound if a plugin
        // emitted more than we reserved -- on the audio thread. Nothing consumes
        // a plugin's note output yet, so the safe option costs nothing.
        let bank = self
            .requests
            .parameters
            .get()
            .expect("initialized before activation");
        let mut feedback = crate::parameters::ParameterOutput(bank);
        let mut events_out = OutputEvents::from_buffer(&mut feedback);
        use clack_host::events::event_types::{TransportEvent, TransportFlags};
        use clack_host::events::EventHeader;
        use clack_host::utils::{BeatTime, SecondsTime};
        let seconds = ctx.transport.sample_pos as f64 / ctx.sample_rate;
        let beats = seconds * ctx.transport.bpm / 60.0;
        let mut flags = TransportFlags::HAS_TEMPO
            | TransportFlags::HAS_BEATS_TIMELINE
            | TransportFlags::HAS_SECONDS_TIMELINE
            | TransportFlags::HAS_TIME_SIGNATURE;
        if ctx.transport.playing {
            flags |= TransportFlags::IS_PLAYING;
        }
        let transport = TransportEvent {
            header: EventHeader::new_core(0, clack_host::events::EventFlags::empty()),
            flags,
            song_pos_beats: BeatTime::from_float(beats),
            song_pos_seconds: SecondsTime::from_float(seconds),
            tempo: ctx.transport.bpm,
            tempo_inc: 0.0,
            loop_start_beats: BeatTime::from_float(0.0),
            loop_end_beats: BeatTime::from_float(0.0),
            loop_start_seconds: SecondsTime::from_float(0.0),
            loop_end_seconds: SecondsTime::from_float(0.0),
            bar_start: BeatTime::from_float((beats / 4.0).floor() * 4.0),
            bar_number: (beats / 4.0).floor() as i32,
            time_signature_numerator: 4,
            time_signature_denominator: 4,
        };
        let status = processor.process(
            &audio_in,
            &mut audio_out,
            &events_in.as_input(),
            &mut events_out,
            Some(*steady_time),
            Some(&transport),
        );
        // Must never decrease and must grow by at least the block size.
        *steady_time = steady_time.wrapping_add(frames as u64);

        self.stats.process_calls.fetch_add(1, Ordering::Relaxed);
        if sent > 0 {
            self.stats.events_sent.fetch_add(sent, Ordering::Relaxed);
        }
        match status {
            Ok(status) => {
                for event in self.events_in.iter() {
                    if let Some(value) = event.as_event::<ParamValueEvent>() {
                        if let Some(id) = value.param_id() {
                            bank.acknowledge(id.get(), value.value());
                        }
                    }
                }
                if status == ProcessStatus::Sleep {
                    self.stats.sleep_requests.fetch_add(1, Ordering::Relaxed);
                }
                let bus = ctx.buffers.as_mut_slices();
                for (destination, processed) in bus.into_iter().zip(self.output.iter()) {
                    destination[..frames].copy_from_slice(&processed[..frames]);
                }
            }
            Err(_) => {
                // The plugin failed: its output buffers hold nothing trustworthy.
                // Leave the bus as it was -- i.e. bypass -- and count it.
                self.stats.process_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl std::fmt::Debug for ClapNode {
    /// Hand-written for the same reason as [`LoadedClapPlugin`]'s: the audio
    /// processor is an FFI handle. Reports the state a test failure needs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapNode")
            .field("live", &self.processor.is_some())
            .field("max_block", &self.max_block)
            .field("stats", &self.stats)
            .finish()
    }
}

impl Drop for ClapNode {
    fn drop(&mut self) {
        let Some(processor) = self.processor.take() else {
            return;
        };
        // Runs on a non-RT thread: either the graph builder (an install that was
        // rejected) or the `basedrop` collector. Never inside the audio callback,
        // because `Owned<CompiledGraph>` defers destructors to the collector.
        if self.retire_tx.send(processor).is_err() {
            // The owning `LoadedClapPlugin` is already gone. Dropping the
            // processor here only releases an `Arc`; clack has already chosen to
            // leak the instance rather than destroy it off the main thread.
            log::warn!(
                "CLAP node retired after its plugin owner was dropped; the plugin instance leaks"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stereo-in/stereo-out effect with no note ports.
    fn stereo_effect_layout() -> ClapPortLayout {
        ClapPortLayout {
            input_ports: 1,
            output_ports: 1,
            main_input_channels: 2,
            main_output_channels: 2,
            note_input_ports: 0,
            note_output_ports: 0,
        }
    }

    #[test]
    fn a_stereo_layout_is_accepted_and_anything_else_is_not() {
        let stereo = stereo_effect_layout();
        assert!(stereo.is_stereo_effect());
        assert!(stereo.is_supported());
        assert!(stereo.has_audio_input());
        assert!(!stereo.has_note_input());

        let mono = ClapPortLayout {
            main_output_channels: 1,
            ..stereo
        };
        assert!(!mono.is_stereo_effect());
        assert!(!mono.is_supported(), "a mono output is still not drivable");

        let no_output = ClapPortLayout {
            output_ports: 0,
            main_output_channels: 0,
            ..stereo
        };
        assert!(!no_output.is_supported());
    }

    #[test]
    fn an_instrument_layout_is_supported_and_is_not_an_effect() {
        let instrument = ClapPortLayout {
            input_ports: 0,
            output_ports: 1,
            main_input_channels: 0,
            main_output_channels: 2,
            note_input_ports: 1,
            note_output_ports: 0,
        };
        assert!(!instrument.is_stereo_effect());
        assert!(instrument.is_instrument());
        assert!(instrument.is_supported());
        assert!(
            !instrument.has_audio_input(),
            "it must not be handed the bus"
        );
        assert!(instrument.has_note_input());

        // A generator with no note input is still an instrument as far as the
        // graph is concerned; it just never receives events.
        let generator = ClapPortLayout {
            note_input_ports: 0,
            ..instrument
        };
        assert!(generator.is_instrument());
        assert!(!generator.has_note_input());

        // Mono out is not something the stereo bus can carry.
        let mono_instrument = ClapPortLayout {
            main_output_channels: 1,
            ..instrument
        };
        assert!(!mono_instrument.is_supported());
    }

    #[test]
    fn a_plugin_with_no_note_ports_extension_reads_as_taking_no_events() {
        // What `read_port_layout` produces for a plugin without the extension.
        let effect = stereo_effect_layout();
        assert_eq!(effect.note_input_ports, 0);
        assert!(
            !effect.has_note_input(),
            "the CLAP spec says no extension means no note ports, so events are skipped"
        );
    }

    #[test]
    fn the_event_buffer_is_warmed_to_capacity_and_left_empty() {
        let buffer = warmed_event_buffer(64);
        assert!(buffer.is_empty(), "warming must not leave events behind");
    }

    #[test]
    fn a_pckn_addresses_one_key_on_note_port_zero_with_a_wildcard_note_id() {
        let target = pckn(3, 69);
        assert_eq!(target.port_index, Match::Specific(0));
        assert_eq!(target.channel, Match::Specific(3));
        assert_eq!(target.key, Match::Specific(69));
        assert_eq!(
            target.note_id,
            Match::All,
            "MYDAW does not track note ids, so a note-off must match every voice"
        );
    }

    /// A shared handler wired to fresh request latches, as `load` would build it.
    fn shared_handler() -> (ClapHostShared, Arc<ClapHostRequests>, Arc<ClapGuiRequests>) {
        let requests = Arc::new(ClapHostRequests::default());
        let gui = Arc::new(ClapGuiRequests::default());
        (
            ClapHostShared {
                requests: Arc::clone(&requests),
                gui: Arc::clone(&gui),
            },
            requests,
            gui,
        )
    }

    #[test]
    fn request_flags_latch_once_and_clear_on_read() {
        let (shared, requests, _) = shared_handler();

        assert!(!requests.take_restart());
        shared.request_restart();
        shared.request_restart();
        assert!(requests.take_restart());
        assert!(!requests.take_restart(), "reading must clear the flag");

        shared.request_process();
        assert!(requests.take_process());
        shared.request_callback();
        assert!(requests.take_callback());
    }

    #[test]
    fn gui_callbacks_latch_the_size_the_plugin_asked_for() {
        let (shared, _, gui) = shared_handler();

        assert!(gui.take_resize().is_none());
        shared
            .request_resize(GuiSize {
                width: 640,
                height: 480,
            })
            .unwrap();
        // A second request before the host looks replaces the first: only the
        // latest size is meaningful.
        shared
            .request_resize(GuiSize {
                width: 800,
                height: 600,
            })
            .unwrap();
        assert_eq!(
            gui.take_resize(),
            Some(GuiSize {
                width: 800,
                height: 600
            })
        );
        assert!(gui.take_resize().is_none(), "reading must clear the flag");
    }

    #[test]
    fn gui_show_hide_and_close_are_latched_separately() {
        let (shared, _, gui) = shared_handler();

        shared.request_show().unwrap();
        shared.request_hide().unwrap();
        shared.resize_hints_changed();
        assert!(gui.take_show());
        assert!(!gui.take_show());
        assert!(gui.take_hide());
        assert!(gui.take_hints_changed());

        assert!(!gui.was_self_destroyed());
        shared.closed(false);
        assert!(gui.take_closed());
        assert!(
            !gui.was_self_destroyed(),
            "closed(false) means the GUI still exists and the host must destroy it"
        );

        shared.closed(true);
        assert!(gui.take_closed());
        assert!(
            gui.was_self_destroyed(),
            "closed(true) means the plugin already destroyed its GUI"
        );

        // Ending a session forgets everything, so nothing leaks into the next one.
        gui.clear();
        assert!(!gui.was_self_destroyed());
        assert!(!gui.take_closed());
    }

    #[test]
    fn a_timer_registration_goes_through_the_main_thread_handler() {
        let timers = Arc::new(ClapTimers::new());
        let mut handler = ClapHostMainThread {
            timers: Arc::clone(&timers),
            state_dirty: Arc::new(AtomicBool::new(false)),
        };

        let id = handler
            .register_timer(33)
            .expect("registration must succeed");
        assert_eq!(timers.len(), 1);
        assert!(timers.contains(id));

        handler.unregister_timer(id).expect("it was registered");
        assert!(timers.is_empty());
        assert!(
            handler.unregister_timer(id).is_err(),
            "unregistering an unknown timer must be reported, not swallowed"
        );
    }

    #[test]
    fn mark_dirty_latches_until_the_host_reads_it() {
        // Step 8 turns this into "the project has unsaved changes"; step 6 only
        // has to prove the callback arrives and clears on read.
        let dirty = Arc::new(AtomicBool::new(false));
        let mut handler = ClapHostMainThread {
            timers: Arc::new(ClapTimers::new()),
            state_dirty: Arc::clone(&dirty),
        };

        assert!(!dirty.load(Ordering::Acquire));
        handler.mark_dirty();
        handler.mark_dirty();
        assert!(dirty.swap(false, Ordering::AcqRel), "latched once");
        assert!(!dirty.load(Ordering::Acquire), "reading clears it");
    }

    #[test]
    fn an_idle_tick_is_reported_as_idle() {
        assert!(MainThreadWork::default().is_idle());
        assert!(!MainThreadWork {
            timers_fired: 1,
            ..MainThreadWork::default()
        }
        .is_idle());
        assert!(!MainThreadWork {
            callback: true,
            ..MainThreadWork::default()
        }
        .is_idle());
    }

    #[test]
    fn an_id_with_an_interior_nul_is_rejected_before_it_reaches_c() {
        let path = std::env::temp_dir().join("mydaw-no-such-bundle.clap");
        let err = LoadedClapPlugin::load(&path, "bad\0id").unwrap_err();
        assert!(
            matches!(err, ClapHostError::InvalidPluginId { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn loading_a_path_that_does_not_exist_is_an_error_not_a_panic() {
        let path = std::env::temp_dir().join("mydaw-definitely-not-a-bundle-77f1.clap");
        assert!(!path.exists());
        let err = LoadedClapPlugin::load(&path, "whatever").unwrap_err();
        assert!(
            matches!(err, ClapHostError::BundleLoad { .. }),
            "got {err:?}"
        );
    }
}
