//! Many CLAP plugins at once: creation, teardown, editors, state.
//!
//! Spikes 2 and 3 hosted **one** plugin, named by an environment variable and
//! owned by a local variable in `run()`. Step 6 makes hosted plugins a
//! first-class part of the project -- an instrument per track, an effect on the
//! master -- so something has to own an arbitrary number of them, keep their
//! ids stable while tracks are renumbered around them, and tear each one down in
//! the order CLAP requires. That is [`ClapInstanceHost`].
//!
//! ```text
//!  main thread (tao's loop)
//!  ------------------------
//!  ClapInstanceHost
//!    ├── #1 LoadedClapPlugin  ──create_node──▶  ClapNode  ─┐
//!    │      └── ClapGuiSession ──▶ native window           │  audio thread
//!    ├── #2 LoadedClapPlugin  ──create_node──▶  ClapNode  ─┤  (the RT graph)
//!    │      └── ClapGuiSession ──▶ native window           │
//!    └── #3 …                                             ─┘
//!            ▲
//!            └── tick(): pump every editor, then give every plugin its
//!                main-thread slice (timers, on_main_thread)
//! ```
//!
//! # Why a `Vec` and not a `HashMap`
//! A session has a handful of plugins, not thousands, and the host is walked in
//! full on every `tick` anyway. A linear scan over ten entries is faster than
//! hashing one, and iteration order stays the creation order, which makes logs
//! and test failures read in the order things happened.
//!
//! # Not `Send`, on purpose
//! [`LoadedClapPlugin`] is `!Send` (CLAP pins instance lifecycle to the main
//! thread) and [`ClapGuiSession`] owns a window handle belonging to the event
//! loop's thread. So this type is `!Send` too, and lives next to the kernel in
//! `run()`. Commands reach it through a mailbox -- see `mydaw_app::clap_bridge`.
//!
//! # Teardown order, which is not negotiable
//! 1. destroy the plugin's editor (a window whose procedure lives in a bundle
//!    that has been unloaded crashes the process on its next message);
//! 2. retire the [`ClapNode`](crate::ClapNode) -- the audio thread has to have
//!    dropped it, and the `basedrop` collector has to have run;
//! 3. deactivate and destroy the instance, on this thread.
//!
//! [`destroy`](ClapInstanceHost::destroy) does all three in that order, and
//! [`shutdown`](ClapInstanceHost::shutdown) does it for every instance.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mydaw_plugin_windows::PluginWindowService;

use crate::gui::{ClapGuiError, ClapGuiSession};
use crate::hosting::{ClapHostError, ClapNode, LoadedClapPlugin, MainThreadWork};
use crate::state::ClapStateError;
use crate::ClapPluginInfo;

/// How long [`ClapInstanceHost::create_node`] waits for a previously installed
/// node to come back from the `basedrop` collector before giving up.
///
/// A CLAP plugin can only be activated once at a time, so re-activating -- which
/// every source swap and every graph rebuild does -- has to wait for the old
/// node's destructor to run on the collector thread. That is one collector sweep
/// (250 ms); this is two of them. Longer would freeze the window for no benefit,
/// because past that point something is genuinely wrong rather than merely slow.
pub const ACTIVATE_WAIT: Duration = Duration::from_millis(500);

/// Handle to one hosted plugin instance.
///
/// Opaque and stable for the instance's whole life. Deliberately *not* the
/// plugin's CLAP id: a session can hold two instances of the same plugin (two
/// tracks of the same synth is the ordinary case), and deliberately not a track
/// index either, because removing a track renumbers every index after it while
/// the instances behind them do not move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClapInstanceId(u64);

impl ClapInstanceId {
    /// The raw number, for logs and for the wire.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ClapInstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "clap#{}", self.0)
    }
}

/// Why an operation on a hosted instance failed.
#[derive(Debug, thiserror::Error)]
pub enum ClapInstanceError {
    /// The id names an instance that has been destroyed, or never existed.
    /// Reachable from a stale UI: a track's editor button clicked just after
    /// its source changed.
    #[error("there is no hosted CLAP instance {0}")]
    NoSuchInstance(ClapInstanceId),

    /// Loading, instantiating or activating the plugin failed.
    #[error(transparent)]
    Host(#[from] ClapHostError),

    /// The editor could not be opened.
    #[error(transparent)]
    Gui(#[from] ClapGuiError),

    /// The state could not be saved or restored.
    #[error(transparent)]
    State(#[from] ClapStateError),

    /// A previously installed node had not been retired in time, so the plugin
    /// could not be activated again. Retryable: the collector is simply behind.
    #[error(
        "the previous audio node of '{id}' was not retired within {} ms, so it cannot be \
         activated again yet",
        ACTIVATE_WAIT.as_millis()
    )]
    StillActive { id: String },
}

/// What the UI needs to know about one instance's editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClapGuiState {
    /// Whether the plugin implements `clap.gui` at all. False means the "Open
    /// editor" button should be absent rather than present and failing.
    pub has_editor: bool,
    /// Whether a window is open right now.
    pub open: bool,
    /// Editor size in physical pixels; `0 x 0` when closed.
    pub width: u32,
    pub height: u32,
}

/// One hosted plugin and the editor session that may belong to it.
struct ClapInstance {
    id: ClapInstanceId,
    plugin: LoadedClapPlugin,
    session: Option<ClapGuiSession>,
}

/// Every CLAP plugin this session has instantiated. **Main thread only.**
///
/// See the module docs for the ownership and teardown story.
pub struct ClapInstanceHost {
    windows: Arc<PluginWindowService>,
    instances: Vec<ClapInstance>,
    next_id: u64,
}

impl ClapInstanceHost {
    /// A host that opens editor windows through `windows`.
    ///
    /// `windows` is thread-affine and refuses calls from any thread but the one
    /// that created it, which is the same constraint this type has.
    pub fn new(windows: Arc<PluginWindowService>) -> Self {
        Self {
            windows,
            instances: Vec::new(),
            next_id: 1,
        }
    }

    /// Load `path` and instantiate the plugin with CLAP id `plugin_id`.
    ///
    /// Every call produces a **new instance**, even of a plugin already hosted:
    /// two tracks of the same synth are two instances with two sets of voices,
    /// which is what a user means by putting it on two tracks.
    ///
    /// # Errors
    /// Anything [`LoadedClapPlugin::load`] reports -- a bundle that will not
    /// load, an id the bundle does not export, a plugin that refuses `init`.
    pub fn create(
        &mut self,
        path: &Path,
        plugin_id: &str,
    ) -> Result<ClapInstanceId, ClapInstanceError> {
        let plugin = LoadedClapPlugin::load(path, plugin_id)?;
        let id = ClapInstanceId(self.next_id);
        self.next_id += 1;
        log::info!(
            "CLAP instance {id} created: '{}' ({}) from {}",
            plugin.info().name,
            plugin.info().id,
            plugin.info().path.display()
        );
        self.instances.push(ClapInstance {
            id,
            plugin,
            session: None,
        });
        Ok(id)
    }

    /// Destroy an instance: close its editor, retire its node, deactivate,
    /// unload.
    ///
    /// Returns `false` for an id that names nothing, which is not an error --
    /// destroying something twice is what a UI that fires two commands does.
    ///
    /// The wait for the node is [`crate::RETIRE_TIMEOUT`] and happens inside
    /// [`LoadedClapPlugin::shutdown`]; it is the only blocking part, and it only
    /// blocks when the plugin was actually in a graph.
    pub fn destroy(&mut self, id: ClapInstanceId) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let mut instance = self.instances.remove(index);

        // The editor first: unloading a bundle while one of its windows still
        // exists is fatal, and the window's procedure lives in that bundle.
        if let Some(session) = instance.session.take() {
            session.close(&mut instance.plugin, &self.windows);
        }
        let clean = instance.plugin.shutdown();
        if clean {
            log::info!(
                "CLAP instance {id} destroyed ('{}')",
                instance.plugin.info().id
            );
        } else {
            log::warn!(
                "CLAP instance {id} ('{}') was still active at teardown; its instance will be \
                 leaked rather than destroyed off the main thread",
                instance.plugin.info().id
            );
        }
        true
    }

    /// Whether `id` names a live instance.
    pub fn contains(&self, id: ClapInstanceId) -> bool {
        self.index_of(id).is_some()
    }

    /// How many instances are hosted.
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// Every live instance id, in creation order.
    pub fn ids(&self) -> Vec<ClapInstanceId> {
        self.instances.iter().map(|instance| instance.id).collect()
    }

    /// Identity of the plugin behind `id`.
    pub fn info(&self, id: ClapInstanceId) -> Option<&ClapPluginInfo> {
        self.instance(id).map(|instance| instance.plugin.info())
    }

    /// The bundle path and CLAP id behind `id` -- what a project file records.
    pub fn locator(&self, id: ClapInstanceId) -> Option<(PathBuf, String)> {
        self.info(id)
            .map(|info| (info.path.clone(), info.id.clone()))
    }

    /// The plugin behind `id`, for callers that need it directly.
    pub fn plugin_mut(&mut self, id: ClapInstanceId) -> Option<&mut LoadedClapPlugin> {
        self.instance_mut(id).map(|instance| &mut instance.plugin)
    }

    /// Activate the instance for this stream and wrap it in an RT node.
    ///
    /// Waits (up to [`ACTIVATE_WAIT`]) for a previously installed node to come
    /// back from the collector first: a plugin can only be activated once, and
    /// swapping a source away and back is exactly the case that needs it. The
    /// wait only happens on that path, and only after the user asked for it.
    ///
    /// # Errors
    /// [`ClapInstanceError::NoSuchInstance`],
    /// [`ClapInstanceError::StillActive`] when the old node never came home, and
    /// anything [`LoadedClapPlugin::create_node`] reports -- most usefully
    /// [`ClapHostError::UnsupportedPortLayout`] for a plugin whose ports this
    /// graph cannot drive.
    pub fn create_node(
        &mut self,
        id: ClapInstanceId,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<ClapNode, ClapInstanceError> {
        let instance = self
            .instance_mut(id)
            .ok_or(ClapInstanceError::NoSuchInstance(id))?;
        if !instance.plugin.wait_for_retire(ACTIVATE_WAIT) {
            return Err(ClapInstanceError::StillActive {
                id: instance.plugin.info().id.clone(),
            });
        }
        Ok(instance.plugin.create_node(sample_rate, max_block)?)
    }

    /// The port layout the plugin reported before activation.
    ///
    /// This is the *authoritative* answer to "may this plugin go here": a
    /// picker filters by the descriptor's features (cheap, see
    /// [`ClapPluginKind`](crate::ClapPluginKind)), and this is what catches a
    /// descriptor that lied.
    pub fn port_layout(&self, id: ClapInstanceId) -> Option<crate::ClapPortLayout> {
        self.instance(id)
            .map(|instance| instance.plugin.port_layout())
    }

    // ------------------------------------------------------------------ state

    /// Serialise the instance. See [`crate::state`].
    ///
    /// # Errors
    /// [`ClapInstanceError::NoSuchInstance`], plus anything
    /// [`LoadedClapPlugin::save_state`] reports.
    pub fn save_state(&mut self, id: ClapInstanceId) -> Result<Vec<u8>, ClapInstanceError> {
        let plugin = self
            .plugin_mut(id)
            .ok_or(ClapInstanceError::NoSuchInstance(id))?;
        Ok(plugin.save_state()?)
    }

    /// Restore an instance from a blob. See [`crate::state`].
    ///
    /// # Errors
    /// [`ClapInstanceError::NoSuchInstance`], plus anything
    /// [`LoadedClapPlugin::load_state`] reports.
    pub fn load_state(
        &mut self,
        id: ClapInstanceId,
        bytes: &[u8],
    ) -> Result<(), ClapInstanceError> {
        let plugin = self
            .plugin_mut(id)
            .ok_or(ClapInstanceError::NoSuchInstance(id))?;
        plugin.load_state(bytes)?;
        Ok(())
    }

    // -------------------------------------------------------------------- gui

    /// Whether the plugin behind `id` implements `clap.gui`.
    pub fn has_editor(&mut self, id: ClapInstanceId) -> bool {
        self.plugin_mut(id)
            .is_some_and(LoadedClapPlugin::has_editor)
    }

    /// Open the instance's editor in its own native window.
    ///
    /// **Several editors may be open at once**, one per instance -- which is the
    /// whole point of this type, and the limitation spike 3 left behind. Asking
    /// for an editor that is already open is not an error: the window the user
    /// wanted is on screen.
    ///
    /// # Errors
    /// [`ClapInstanceError::NoSuchInstance`], and [`ClapGuiError::NoGuiSupport`]
    /// for a plugin with no editor -- a normal answer, not a malfunction.
    pub fn open_gui(
        &mut self,
        id: ClapInstanceId,
        title: &str,
    ) -> Result<ClapGuiState, ClapInstanceError> {
        let windows = Arc::clone(&self.windows);
        let instance = self
            .instance_mut(id)
            .ok_or(ClapInstanceError::NoSuchInstance(id))?;
        if instance.session.is_some() {
            return Ok(gui_state(instance));
        }

        let session = ClapGuiSession::open(&mut instance.plugin, &windows, title)?;
        log::info!(
            "plugin editor open: {id} '{}' at {}x{} (scale {})",
            instance.plugin.info().id,
            session.size().width,
            session.size().height,
            session.scale()
        );
        instance.session = Some(session);
        Ok(gui_state(instance))
    }

    /// Close one instance's editor. Not an error when nothing is open.
    pub fn close_gui(&mut self, id: ClapInstanceId) -> ClapGuiState {
        let windows = Arc::clone(&self.windows);
        let Some(instance) = self.instance_mut(id) else {
            return ClapGuiState::default();
        };
        if let Some(session) = instance.session.take() {
            session.close(&mut instance.plugin, &windows);
            log::info!("plugin editor closed: {id} '{}'", instance.plugin.info().id);
        }
        gui_state(instance)
    }

    /// The editor state of one instance, for the UI.
    pub fn gui_state(&mut self, id: ClapInstanceId) -> Option<ClapGuiState> {
        self.instance_mut(id).map(gui_state)
    }

    /// The native window handle of one instance's editor, if it has one open.
    ///
    /// For diagnostics and for the smoke tests, which drive the window the way a
    /// user's mouse would (a programmatic `SetWindowPos`) and then check what the
    /// plugin painted.
    pub fn editor_hwnd(&self, id: ClapInstanceId) -> Option<isize> {
        self.instance(id)
            .and_then(|instance| instance.session.as_ref())
            .map(ClapGuiSession::hwnd)
    }

    /// The size one instance's editor has negotiated, if it is open.
    pub fn editor_size(&self, id: ClapInstanceId) -> Option<(u32, u32)> {
        self.instance(id)
            .and_then(|instance| instance.session.as_ref())
            .map(|session| (session.size().width, session.size().height))
    }

    /// How many editors are open. Diagnostics and tests.
    pub fn open_editors(&self) -> usize {
        self.instances
            .iter()
            .filter(|instance| instance.session.is_some())
            .count()
    }

    // ------------------------------------------------------------------- pump

    /// One pass of the host's per-frame obligations, for **every** instance.
    ///
    /// Called from the application's event loop (`MainEventsCleared`). For each
    /// instance: service its editor if one is open, closing it when the user or
    /// the plugin asked; then give the plugin its main-thread slice -- due
    /// `clap.timer-support` timers and `on_main_thread` -- which is owed whether
    /// or not an editor is open.
    ///
    /// Returns the summed work done, so a test can assert the pump is alive
    /// rather than assume it.
    pub fn tick(&mut self) -> MainThreadWork {
        let windows = Arc::clone(&self.windows);
        let mut total = MainThreadWork::default();
        let mut closing: Vec<ClapInstanceId> = Vec::new();

        for instance in &mut self.instances {
            if let Some(session) = instance.session.as_mut() {
                if session.pump(&mut instance.plugin, &windows).close_requested {
                    closing.push(instance.id);
                }
            }
            let work = instance.plugin.tick_main_thread();
            total.timers_fired += work.timers_fired;
            total.callback |= work.callback;
            total.restart_requested |= work.restart_requested;
        }

        // Closing outside the loop: `close_gui` needs `&mut self`, and a plugin
        // is allowed to ask its editor to close from inside its own pump.
        for id in closing {
            self.close_gui(id);
        }
        total
    }

    /// Deactivate every instance that has a node waiting to come home.
    ///
    /// Cheap and non-blocking; call it whenever convenient (the shell calls it
    /// after retiring a graph) so a later `create_node` does not have to wait.
    /// Returns how many processors were collected.
    pub fn collect_retired(&mut self) -> usize {
        self.instances
            .iter_mut()
            .map(|instance| instance.plugin.collect_retired())
            .sum()
    }

    /// Close every editor and destroy every instance, in the required order.
    ///
    /// The shell calls this on `RunEvent::Exit`, **before** the kernel stops:
    /// audio must already have been stopped so the graph -- and the nodes in it
    /// -- can be retired.
    pub fn shutdown(&mut self) {
        for id in self.ids() {
            self.destroy(id);
        }
        let closed = self.windows.close_all();
        if closed > 0 {
            log::debug!("closed {closed} leftover plugin window(s) at shutdown");
        }
    }

    fn index_of(&self, id: ClapInstanceId) -> Option<usize> {
        self.instances.iter().position(|instance| instance.id == id)
    }

    fn instance(&self, id: ClapInstanceId) -> Option<&ClapInstance> {
        self.instances.iter().find(|instance| instance.id == id)
    }

    fn instance_mut(&mut self, id: ClapInstanceId) -> Option<&mut ClapInstance> {
        self.instances.iter_mut().find(|instance| instance.id == id)
    }
}

impl std::fmt::Debug for ClapInstanceHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapInstanceHost")
            .field("instances", &self.instances.len())
            .field("open_editors", &self.open_editors())
            .field(
                "ids",
                &self
                    .instances
                    .iter()
                    .map(|instance| (instance.id, instance.plugin.info().id.as_str()))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Drop for ClapInstanceHost {
    /// Last chance to tear down on the right thread. `shutdown` is idempotent,
    /// so a shell that already called it pays nothing here.
    fn drop(&mut self) {
        if !self.instances.is_empty() {
            log::debug!(
                "ClapInstanceHost dropped with {} live instance(s); tearing them down",
                self.instances.len()
            );
        }
        self.shutdown();
    }
}

fn gui_state(instance: &mut ClapInstance) -> ClapGuiState {
    let has_editor = instance.plugin.has_editor();
    match instance.session.as_ref() {
        Some(session) => ClapGuiState {
            has_editor,
            open: true,
            width: session.size().width,
            height: session.size().height,
        },
        None => ClapGuiState {
            has_editor,
            ..ClapGuiState::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> ClapInstanceHost {
        ClapInstanceHost::new(Arc::new(PluginWindowService::new()))
    }

    #[test]
    fn a_fresh_host_owns_nothing_and_has_no_editors() {
        let host = host();
        assert!(host.is_empty());
        assert_eq!(host.len(), 0);
        assert_eq!(host.open_editors(), 0);
        assert!(host.ids().is_empty());
    }

    #[test]
    fn instance_ids_are_unique_stable_and_never_reused() {
        // A destroyed instance's id must not come back on the next create: a
        // stale UI holding it would otherwise silently address a different
        // plugin.
        let mut host = host();
        host.next_id = 7;
        let first = ClapInstanceId(host.next_id);
        host.next_id += 1;
        let second = ClapInstanceId(host.next_id);
        assert_ne!(first, second);
        assert_eq!(first.get(), 7);
        assert_eq!(first.to_string(), "clap#7");
    }

    #[test]
    fn an_unknown_id_is_named_rather_than_ignored() {
        let mut host = host();
        let ghost = ClapInstanceId(42);
        assert!(!host.contains(ghost));
        assert!(host.info(ghost).is_none());
        assert!(host.locator(ghost).is_none());
        assert!(host.port_layout(ghost).is_none());
        assert!(host.gui_state(ghost).is_none());
        assert!(!host.has_editor(ghost));
        assert!(
            !host.destroy(ghost),
            "destroying nothing is not an error, but it must say so"
        );

        let err = host.save_state(ghost).unwrap_err();
        assert!(matches!(err, ClapInstanceError::NoSuchInstance(id) if id == ghost));
        assert!(err.to_string().contains("clap#42"), "{err}");
        assert!(matches!(
            host.load_state(ghost, b"x"),
            Err(ClapInstanceError::NoSuchInstance(_))
        ));
        assert!(matches!(
            host.create_node(ghost, 48_000.0, 256),
            Err(ClapInstanceError::NoSuchInstance(_))
        ));
        assert!(matches!(
            host.open_gui(ghost, "nothing"),
            Err(ClapInstanceError::NoSuchInstance(_))
        ));
        assert_eq!(host.close_gui(ghost), ClapGuiState::default());
    }

    #[test]
    fn creating_from_a_path_that_is_not_a_bundle_is_a_typed_error_not_a_panic() {
        let mut host = host();
        let missing = std::env::temp_dir().join("mydaw-no-such-bundle-7c31.clap");
        let err = host.create(&missing, "com.acme.gone").unwrap_err();
        assert!(
            matches!(
                err,
                ClapInstanceError::Host(ClapHostError::BundleLoad { .. })
            ),
            "got {err:?}"
        );
        assert!(host.is_empty(), "a failed create must add nothing");
    }

    #[test]
    fn ticking_and_collecting_with_nothing_hosted_do_nothing() {
        let mut host = host();
        assert!(host.tick().is_idle());
        assert_eq!(host.collect_retired(), 0);
        host.shutdown();
        assert!(host.is_empty());
    }

    #[test]
    fn the_activate_wait_is_a_couple_of_collector_sweeps() {
        // The collector sweeps every 250 ms, so anything shorter would make a
        // perfectly normal source swap fail; anything much longer would freeze
        // the window while something is genuinely wrong.
        assert_eq!(ACTIVATE_WAIT, Duration::from_millis(500));
    }

    #[test]
    fn a_gui_state_with_no_session_is_closed_but_still_reports_whether_one_exists() {
        // The UI needs the difference: a plugin with no editor should have no
        // button, and a plugin with a closed editor should have one that opens.
        let closed = ClapGuiState {
            has_editor: true,
            ..ClapGuiState::default()
        };
        assert!(!closed.open);
        assert_eq!((closed.width, closed.height), (0, 0));
        assert!(!ClapGuiState::default().has_editor);
    }
}
