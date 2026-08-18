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
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use clack_extensions::audio_ports::PluginAudioPorts;
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::prelude::*;
use mydaw_rt_core::{ProcessCtx, RtNode, CHANNELS};

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
    restart: AtomicBool,
    process: AtomicBool,
    callback: AtomicBool,
}

impl ClapHostRequests {
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
}

impl MainThreadHandler<'_> for ClapHostMainThread {}

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
    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &ClapHostShared) {
        builder.register::<HostGui>();
        builder.register::<HostTimer>();
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

/// What a plugin says about its main audio ports.
///
/// Read once, on the main thread, before activation: CLAP only allows a plugin to
/// change its port configuration while deactivated, so a layout read here stays
/// valid for the life of the activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClapPortLayout {
    pub input_ports: u32,
    pub output_ports: u32,
    /// Channel count of input port 0.
    pub main_input_channels: u32,
    /// Channel count of output port 0.
    pub main_output_channels: u32,
}

impl ClapPortLayout {
    /// Whether this layout is the stereo-in/stereo-out shape the Phase 1 graph
    /// can drive. Everything else is a Phase 2 mixer feature.
    pub fn is_stereo_effect(&self) -> bool {
        self.input_ports >= 1
            && self.output_ports >= 1
            && self.main_input_channels == CHANNELS as u32
            && self.main_output_channels == CHANNELS as u32
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
    instance: PluginInstance<MydawClapHost>,
    info: ClapPluginInfo,
    layout: ClapPortLayout,
    requests: Arc<ClapHostRequests>,
    gui_requests: Arc<ClapGuiRequests>,
    timers: Arc<ClapTimers>,
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
            let gui_requests = Arc::new(ClapGuiRequests::default());
            let timers = Arc::new(ClapTimers::new());
            let shared_requests = Arc::clone(&requests);
            let shared_gui = Arc::clone(&gui_requests);
            let main_thread_timers = Arc::clone(&timers);
            let instance = PluginInstance::<MydawClapHost>::new(
                move |_| ClapHostShared {
                    requests: shared_requests,
                    gui: shared_gui,
                },
                move |_| ClapHostMainThread {
                    timers: main_thread_timers,
                },
                &entry,
                &id,
                &host_info,
            )
            .map_err(|err| ClapHostError::Instantiate {
                id: plugin_id.to_owned(),
                message: err.to_string(),
            })?;

            Ok((entry, instance, info, requests, gui_requests, timers))
        }))
        .map_err(|_| ClapHostError::Panicked { stage: "loading" })??;

        let (entry, mut instance, info, requests, gui_requests, timers) = loaded;

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

        if !self.layout.is_stereo_effect() {
            return Err(ClapHostError::UnsupportedPortLayout {
                message: format!(
                    "'{}' reports {} input port(s) x {} ch and {} output port(s) x {} ch; \
                     the Phase 1 graph drives stereo in / stereo out only",
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

/// Read the plugin's main port layout through the `audio-ports` extension.
///
/// A plugin that does not implement `audio-ports` has, per the CLAP
/// specification, *no* audio ports at all -- so refusing it is correct, not
/// pedantic: handing buffers to a plugin that declared none is a host bug.
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

        Ok(ClapPortLayout {
            input_ports,
            output_ports,
            main_input_channels,
            main_output_channels,
        })
    }))
    .map_err(|_| ClapHostError::Panicked {
        stage: "reporting its audio ports",
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
            steady_time,
            ..
        } = self;
        let Some(processor) = processor.as_mut() else {
            // Retired node still in a live graph: pass audio through untouched.
            return;
        };

        let (input_left, input_right) = input.split_at_mut(1);
        let audio_in = input_ports.with_input_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_input_only(
                [
                    InputChannel::variable(&mut input_left[0][..frames]),
                    InputChannel::variable(&mut input_right[0][..frames]),
                ]
                .into_iter(),
            ),
        }]);

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

        let mut events_out = OutputEvents::void();
        let status = processor.process(
            &audio_in,
            &mut audio_out,
            &InputEvents::empty(),
            &mut events_out,
            Some(*steady_time),
            None,
        );
        // Must never decrease and must grow by at least the block size.
        *steady_time = steady_time.wrapping_add(frames as u64);

        self.stats.process_calls.fetch_add(1, Ordering::Relaxed);
        match status {
            Ok(status) => {
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

    #[test]
    fn a_stereo_layout_is_accepted_and_anything_else_is_not() {
        let stereo = ClapPortLayout {
            input_ports: 1,
            output_ports: 1,
            main_input_channels: 2,
            main_output_channels: 2,
        };
        assert!(stereo.is_stereo_effect());

        let instrument = ClapPortLayout {
            input_ports: 0,
            main_input_channels: 0,
            ..stereo
        };
        assert!(!instrument.is_stereo_effect());

        let mono = ClapPortLayout {
            main_output_channels: 1,
            ..stereo
        };
        assert!(!mono.is_stereo_effect());
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
