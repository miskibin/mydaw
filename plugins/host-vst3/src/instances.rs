//! Many VST3 plugins at once: creation, teardown, editors, state.
//!
//! The direct counterpart of `mydaw-plugin-host-clap`'s `ClapInstanceHost`, and
//! deliberately the same shape -- a `Vec` of instances keyed by an opaque id, one
//! `tick` that services every editor, and one teardown order that is not
//! negotiable:
//!
//! 1. remove the plugin's editor from its window (a window whose procedure lives
//!    in a module that has been unloaded crashes the process on its next
//!    message);
//! 2. retire the [`Vst3Node`](crate::Vst3Node) -- the audio thread has to have
//!    dropped it, and the `basedrop` collector has to have run;
//! 3. deactivate, terminate and unload, on this thread.
//!
//! [`destroy`](Vst3InstanceHost::destroy) does all three in that order, and
//! [`shutdown`](Vst3InstanceHost::shutdown) does it for every instance.
//!
//! # Not `Send`, on purpose
//! [`LoadedVst3Plugin`] owns COM pointers and a loaded module, and
//! [`Vst3GuiSession`] owns a window handle belonging to the event loop's thread.
//! So this type is `!Send` too, and lives next to the kernel in the shell's
//! `run()`. Commands reach it through a mailbox -- see `mydaw_app::vst3_bridge`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mydaw_plugin_windows::PluginWindowService;

use crate::gui::{Vst3GuiError, Vst3GuiSession};
use crate::hosting::{LoadedVst3Plugin, Vst3BusLayout, Vst3HostError, Vst3Node};
use crate::module::ClassId;
use crate::scan::Vst3PluginInfo;

/// How long [`Vst3InstanceHost::create_node`] waits for a previously installed
/// node to come back from the `basedrop` collector before giving up.
///
/// A VST3 component can only be active once at a time, so re-activating -- which
/// every source swap and every graph rebuild does -- has to wait for the old
/// node's destructor to run on the collector thread. That is one collector sweep
/// (250 ms); this is two of them.
pub const ACTIVATE_WAIT: Duration = Duration::from_millis(500);

/// Handle to one hosted plugin instance.
///
/// Opaque and stable for the instance's whole life. Deliberately *not* the
/// plugin's class id: a session can hold two instances of the same plugin, and
/// deliberately not a track index either, because removing a track renumbers
/// every index after it while the instances behind them do not move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Vst3InstanceId(u64);

impl Vst3InstanceId {
    /// The raw number, for logs and for the wire.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for Vst3InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vst3#{}", self.0)
    }
}

/// Why an operation on a hosted instance failed.
#[derive(Debug, thiserror::Error)]
pub enum Vst3InstanceError {
    /// The id names an instance that has been destroyed, or never existed.
    /// Reachable from a stale UI: a track's editor button clicked just after its
    /// source changed.
    #[error("there is no hosted VST3 instance {0}")]
    NoSuchInstance(Vst3InstanceId),

    /// Loading, instantiating or activating the plugin failed.
    #[error(transparent)]
    Host(#[from] Vst3HostError),

    /// The editor could not be opened.
    #[error(transparent)]
    Gui(#[from] Vst3GuiError),

    /// A previously installed node had not been retired in time, so the plugin
    /// could not be activated again. Retryable: the collector is simply behind.
    #[error(
        "the previous audio node of '{name}' was not retired within {} ms, so it cannot be \
         activated again yet",
        ACTIVATE_WAIT.as_millis()
    )]
    StillActive { name: String },
}

/// What the UI needs to know about one instance's editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Vst3GuiState {
    /// Whether the plugin has an edit controller at all. False means the "Open
    /// editor" button should be absent rather than present and failing.
    pub has_editor: bool,
    /// Whether a window is open right now.
    pub open: bool,
    /// Editor size in physical pixels; `0 x 0` when closed.
    pub width: u32,
    pub height: u32,
}

/// One hosted plugin and the editor session that may belong to it.
struct Vst3Instance {
    id: Vst3InstanceId,
    plugin: LoadedVst3Plugin,
    session: Option<Vst3GuiSession>,
}

/// Every VST3 plugin this session has instantiated. **Main thread only.**
pub struct Vst3InstanceHost {
    windows: Arc<PluginWindowService>,
    instances: Vec<Vst3Instance>,
    next_id: u64,
}

impl Vst3InstanceHost {
    /// A host that opens editor windows through `windows`.
    pub fn new(windows: Arc<PluginWindowService>) -> Self {
        Self {
            windows,
            instances: Vec::new(),
            next_id: 1,
        }
    }

    /// Load `path` and instantiate the plugin class `cid`.
    ///
    /// Every call produces a **new instance**, even of a plugin already hosted.
    ///
    /// # Errors
    /// Anything [`LoadedVst3Plugin::load`] reports.
    pub fn create(
        &mut self,
        path: &Path,
        cid: ClassId,
    ) -> Result<Vst3InstanceId, Vst3InstanceError> {
        let plugin = LoadedVst3Plugin::load(path, cid)?;
        let id = Vst3InstanceId(self.next_id);
        self.next_id += 1;
        log::info!(
            "VST3 instance {id} created: '{}' ({}) from {} [{}]",
            plugin.info().name,
            plugin.info().cid,
            plugin.info().path.display(),
            if plugin.is_single_component() {
                "single-component"
            } else {
                "dual-component"
            }
        );
        self.instances.push(Vst3Instance {
            id,
            plugin,
            session: None,
        });
        Ok(id)
    }

    /// Destroy an instance: close its editor, retire its node, deactivate,
    /// terminate, unload.
    ///
    /// Returns `false` for an id that names nothing, which is not an error --
    /// destroying something twice is what a UI that fires two commands does.
    pub fn destroy(&mut self, id: Vst3InstanceId) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let mut instance = self.instances.remove(index);

        // The editor first: unloading a module while one of its windows still
        // exists is fatal, and the window's procedure lives in that module.
        if let Some(session) = instance.session.take() {
            session.close(&mut instance.plugin, &self.windows);
        }
        let clean = instance.plugin.shutdown();
        if clean {
            log::info!(
                "VST3 instance {id} destroyed ('{}')",
                instance.plugin.info().name
            );
        } else {
            log::warn!(
                "VST3 instance {id} ('{}') was still active at teardown; its module will be \
                 leaked rather than unloaded",
                instance.plugin.info().name
            );
        }
        true
    }

    /// Whether `id` names a live instance.
    pub fn contains(&self, id: Vst3InstanceId) -> bool {
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
    pub fn ids(&self) -> Vec<Vst3InstanceId> {
        self.instances.iter().map(|instance| instance.id).collect()
    }

    /// Identity of the plugin behind `id`.
    pub fn info(&self, id: Vst3InstanceId) -> Option<&Vst3PluginInfo> {
        self.instance(id).map(|instance| instance.plugin.info())
    }

    /// The module path and class id behind `id` -- what a project file records.
    pub fn locator(&self, id: Vst3InstanceId) -> Option<(PathBuf, ClassId)> {
        self.info(id).map(|info| (info.path.clone(), info.cid))
    }

    /// The plugin behind `id`, for callers that need it directly.
    pub fn plugin_mut(&mut self, id: Vst3InstanceId) -> Option<&mut LoadedVst3Plugin> {
        self.instance_mut(id).map(|instance| &mut instance.plugin)
    }

    /// The bus layout the plugin reported before activation.
    ///
    /// This is the *authoritative* answer to "may this plugin go here": a picker
    /// filters by the class's `subCategories` (cheap, see
    /// [`Vst3PluginKind`](crate::Vst3PluginKind)), and this is what catches a
    /// class that lied.
    pub fn bus_layout(&self, id: Vst3InstanceId) -> Option<Vst3BusLayout> {
        self.instance(id)
            .map(|instance| instance.plugin.bus_layout())
    }

    /// Activate the instance for this stream and wrap it in an RT node.
    ///
    /// Waits (up to [`ACTIVATE_WAIT`]) for a previously installed node to come
    /// back from the collector first.
    ///
    /// # Errors
    /// [`Vst3InstanceError::NoSuchInstance`], [`Vst3InstanceError::StillActive`]
    /// when the old node never came home, and anything
    /// [`LoadedVst3Plugin::create_node`] reports.
    pub fn create_node(
        &mut self,
        id: Vst3InstanceId,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Vst3Node, Vst3InstanceError> {
        let instance = self
            .instance_mut(id)
            .ok_or(Vst3InstanceError::NoSuchInstance(id))?;
        if !instance.plugin.wait_for_retire(ACTIVATE_WAIT) {
            return Err(Vst3InstanceError::StillActive {
                name: instance.plugin.info().name.clone(),
            });
        }
        Ok(instance.plugin.create_node(sample_rate, max_block)?)
    }

    // ------------------------------------------------------------------ state

    /// Serialise the instance's component state.
    ///
    /// # Errors
    /// [`Vst3InstanceError::NoSuchInstance`], plus anything
    /// [`LoadedVst3Plugin::save_state`] reports.
    pub fn save_state(&mut self, id: Vst3InstanceId) -> Result<Vec<u8>, Vst3InstanceError> {
        let plugin = self
            .plugin_mut(id)
            .ok_or(Vst3InstanceError::NoSuchInstance(id))?;
        Ok(plugin.save_state()?)
    }

    /// Restore an instance from a blob.
    ///
    /// # Errors
    /// [`Vst3InstanceError::NoSuchInstance`], plus anything
    /// [`LoadedVst3Plugin::load_state`] reports.
    pub fn load_state(
        &mut self,
        id: Vst3InstanceId,
        bytes: &[u8],
    ) -> Result<(), Vst3InstanceError> {
        let plugin = self
            .plugin_mut(id)
            .ok_or(Vst3InstanceError::NoSuchInstance(id))?;
        plugin.load_state(bytes)?;
        Ok(())
    }

    // -------------------------------------------------------------------- gui

    /// Whether the plugin behind `id` has an edit controller.
    ///
    /// A controller is not a guarantee of a view -- `createView` may still return
    /// null -- but its absence is a guarantee of *no* view, which is what the UI
    /// needs to decide whether to draw a button.
    pub fn has_editor(&mut self, id: Vst3InstanceId) -> bool {
        self.instance(id)
            .is_some_and(|instance| instance.plugin.controller_present())
    }

    /// Open the instance's editor in its own native window.
    ///
    /// Several editors may be open at once, one per instance. Asking for an
    /// editor that is already open is not an error: the window the user wanted is
    /// on screen.
    ///
    /// # Errors
    /// [`Vst3InstanceError::NoSuchInstance`], and [`Vst3GuiError::NoEditor`] for
    /// a plugin with none -- a normal answer, not a malfunction.
    pub fn open_gui(
        &mut self,
        id: Vst3InstanceId,
        title: &str,
    ) -> Result<Vst3GuiState, Vst3InstanceError> {
        let windows = Arc::clone(&self.windows);
        let instance = self
            .instance_mut(id)
            .ok_or(Vst3InstanceError::NoSuchInstance(id))?;
        if instance.session.is_some() {
            return Ok(gui_state(instance));
        }

        let session = Vst3GuiSession::open(&mut instance.plugin, &windows, title)?;
        log::info!(
            "plugin editor open: {id} '{}' at {}x{} (scale {})",
            instance.plugin.info().name,
            session.size().width,
            session.size().height,
            session.scale()
        );
        instance.session = Some(session);
        Ok(gui_state(instance))
    }

    /// Close one instance's editor. Not an error when nothing is open.
    pub fn close_gui(&mut self, id: Vst3InstanceId) -> Vst3GuiState {
        let windows = Arc::clone(&self.windows);
        let Some(instance) = self.instance_mut(id) else {
            return Vst3GuiState::default();
        };
        if let Some(session) = instance.session.take() {
            session.close(&mut instance.plugin, &windows);
            log::info!(
                "plugin editor closed: {id} '{}'",
                instance.plugin.info().name
            );
        }
        gui_state(instance)
    }

    /// The editor state of one instance, for the UI.
    pub fn gui_state(&mut self, id: Vst3InstanceId) -> Option<Vst3GuiState> {
        self.instance_mut(id).map(|instance| gui_state(instance))
    }

    /// The native window handle of one instance's editor, if it has one open.
    ///
    /// For diagnostics and for the smoke tests, which drive the window the way a
    /// user's mouse would and then check what the plugin painted.
    pub fn editor_hwnd(&self, id: Vst3InstanceId) -> Option<isize> {
        self.instance(id)
            .and_then(|instance| instance.session.as_ref())
            .map(Vst3GuiSession::hwnd)
    }

    /// The size one instance's editor has negotiated, if it is open.
    pub fn editor_size(&self, id: Vst3InstanceId) -> Option<(u32, u32)> {
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
    /// Called from the application's event loop (`MainEventsCleared`): service
    /// each open editor, closing it when the user asked, and collect any node
    /// the collector has returned.
    ///
    /// Returns how many editors asked to close, so a test can assert the pump is
    /// alive rather than assume it.
    pub fn tick(&mut self) -> usize {
        let windows = Arc::clone(&self.windows);
        let mut closing: Vec<Vst3InstanceId> = Vec::new();

        for instance in &mut self.instances {
            instance.plugin.sync_parameter_feedback();
            if let Some(session) = instance.session.as_mut() {
                if session.pump(&mut instance.plugin, &windows).close_requested {
                    closing.push(instance.id);
                }
            }
            // Cheap: one non-blocking channel poll per instance. Doing it here
            // means a later `create_node` rarely has to wait at all.
            instance.plugin.collect_retired();
        }

        let closed = closing.len();
        // Closing outside the loop: `close_gui` needs `&mut self`.
        for id in closing {
            self.close_gui(id);
        }
        closed
    }

    pub fn take_editor_changes(&self) -> bool {
        self.instances.iter().fold(false, |changed, instance| {
            instance.plugin.requests().take_dirty() | changed
        })
    }

    /// Deactivate every instance that has a node waiting to come home.
    ///
    /// Cheap and non-blocking. Returns how many processors were collected.
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
    }

    fn index_of(&self, id: Vst3InstanceId) -> Option<usize> {
        self.instances.iter().position(|instance| instance.id == id)
    }

    fn instance(&self, id: Vst3InstanceId) -> Option<&Vst3Instance> {
        self.instances.iter().find(|instance| instance.id == id)
    }

    fn instance_mut(&mut self, id: Vst3InstanceId) -> Option<&mut Vst3Instance> {
        self.instances.iter_mut().find(|instance| instance.id == id)
    }
}

impl std::fmt::Debug for Vst3InstanceHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3InstanceHost")
            .field("instances", &self.instances.len())
            .field("open_editors", &self.open_editors())
            .field(
                "ids",
                &self
                    .instances
                    .iter()
                    .map(|instance| (instance.id, instance.plugin.info().name.clone()))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Drop for Vst3InstanceHost {
    /// Last chance to tear down on the right thread. `shutdown` is idempotent,
    /// so a shell that already called it pays nothing here.
    fn drop(&mut self) {
        if !self.instances.is_empty() {
            log::debug!(
                "Vst3InstanceHost dropped with {} live instance(s); tearing them down",
                self.instances.len()
            );
        }
        self.shutdown();
    }
}

fn gui_state(instance: &Vst3Instance) -> Vst3GuiState {
    let has_editor = instance.plugin.controller_present();
    match instance.session.as_ref() {
        Some(session) => Vst3GuiState {
            has_editor,
            open: true,
            width: session.size().width,
            height: session.size().height,
        },
        None => Vst3GuiState {
            has_editor,
            ..Vst3GuiState::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Vst3InstanceHost {
        Vst3InstanceHost::new(Arc::new(PluginWindowService::new()))
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
        let first = Vst3InstanceId(host.next_id);
        host.next_id += 1;
        let second = Vst3InstanceId(host.next_id);
        assert_ne!(first, second);
        assert_eq!(first.get(), 7);
        assert_eq!(first.to_string(), "vst3#7");
    }

    #[test]
    fn an_unknown_id_is_named_rather_than_ignored() {
        let mut host = host();
        let ghost = Vst3InstanceId(42);
        assert!(!host.contains(ghost));
        assert!(host.info(ghost).is_none());
        assert!(host.locator(ghost).is_none());
        assert!(host.bus_layout(ghost).is_none());
        assert!(host.gui_state(ghost).is_none());
        assert!(!host.has_editor(ghost));
        assert!(host.editor_hwnd(ghost).is_none());
        assert!(host.editor_size(ghost).is_none());
        assert!(
            !host.destroy(ghost),
            "destroying nothing is not an error, but it must say so"
        );

        let err = host.save_state(ghost).unwrap_err();
        assert!(matches!(err, Vst3InstanceError::NoSuchInstance(id) if id == ghost));
        assert!(err.to_string().contains("vst3#42"), "{err}");
        assert!(matches!(
            host.load_state(ghost, b"x"),
            Err(Vst3InstanceError::NoSuchInstance(_))
        ));
        assert!(matches!(
            host.create_node(ghost, 48_000.0, 256),
            Err(Vst3InstanceError::NoSuchInstance(_))
        ));
        assert!(matches!(
            host.open_gui(ghost, "nothing"),
            Err(Vst3InstanceError::NoSuchInstance(_))
        ));
        assert_eq!(host.close_gui(ghost), Vst3GuiState::default());
    }

    #[test]
    fn creating_from_a_path_that_is_not_a_module_is_a_typed_error_not_a_panic() {
        let mut host = host();
        let missing = std::env::temp_dir().join("mydaw-no-such-module-5b22.vst3");
        let err = host.create(&missing, ClassId([0; 16])).unwrap_err();
        assert!(
            matches!(err, Vst3InstanceError::Host(Vst3HostError::Module(_))),
            "got {err:?}"
        );
        assert!(host.is_empty(), "a failed create must add nothing");
    }

    #[test]
    fn ticking_and_collecting_with_nothing_hosted_do_nothing() {
        let mut host = host();
        assert_eq!(host.tick(), 0);
        assert_eq!(host.collect_retired(), 0);
        host.shutdown();
        assert!(host.is_empty());
    }

    #[test]
    fn the_activate_wait_is_a_couple_of_collector_sweeps() {
        // The collector sweeps every 250 ms, so anything shorter would make a
        // perfectly normal source swap fail.
        assert_eq!(ACTIVATE_WAIT, Duration::from_millis(500));
    }

    #[test]
    fn a_gui_state_with_no_session_is_closed_but_still_reports_whether_one_exists() {
        // The UI needs the difference: a plugin with no editor should have no
        // button, and a plugin with a closed editor should have one that opens.
        let closed = Vst3GuiState {
            has_editor: true,
            ..Vst3GuiState::default()
        };
        assert!(!closed.open);
        assert_eq!((closed.width, closed.height), (0, 0));
        assert!(!Vst3GuiState::default().has_editor);
    }
}
