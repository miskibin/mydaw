//! `mydaw-plugin-host-vst3` -- VST3 plugin discovery and hosting.
//!
//! The compatibility half of MYDAW's plugin story. CLAP is the primary format and
//! `mydaw-plugin-host-clap` is the reference implementation; this crate is the
//! same architecture over a much older API, and the architecture is deliberately
//! identical so that the two can be read side by side:
//!
//! | | CLAP | VST3 |
//! | --- | --- | --- |
//! | discovery | `ClapHostService` | [`Vst3HostService`] |
//! | one instance | `LoadedClapPlugin` | [`LoadedVst3Plugin`] |
//! | in the graph | `ClapNode` | [`Vst3Node`] |
//! | its editor | `ClapGuiSession` | [`Vst3GuiSession`] |
//! | many at once | `ClapInstanceHost` | [`Vst3InstanceHost`] |
//!
//! Where they differ, the difference is the format's, not a choice: VST3 has no
//! safe host binding, so [`module`] and [`host`] exist to build one, and every
//! COM call in this crate is `unsafe` with the invariant it relies on written
//! next to it.
//!
//! # Scope -- this is a spike
//!
//! Step 7 proves the pillar: scan → load → process → notes → GUI, on the real
//! plugins installed on a real machine. It is **not** at parity with the CLAP
//! path. What is missing, and why, is in
//! `docs/superpowers/specs/2026-08-19-step7-vst3-host.md`; the short list is
//! parameters (none are exposed) and `IConnectionPoint` messaging (a
//! dual-component plugin's editor may not follow its processor).
//!
//! # Crash isolation -- read this before trusting a scan
//!
//! Opening a VST3 module runs third-party code: the OS loader runs the DLL's
//! entry point, then `InitDll` runs the vendor's initialisation. That code can
//! crash, and on a machine with licence-protected plugins it can also *stop* -- a
//! modal dialog waiting for a human is not a crash and no amount of
//! `catch_unwind` sees it.
//!
//! **A scan therefore does not open modules in MYDAW's process.**
//! [`Vst3HostService::scan_detailed`] hands the module list to
//! [`mydaw_plugin_scan::ModuleScan`], which:
//!
//! - answers from a **metadata cache** for every module whose binary has not
//!   changed since last time -- nothing is loaded at all, which is why the second
//!   scan of a machine is instant;
//! - opens every other module in a **child process** with a deadline. A segfault,
//!   an `abort()`, a stack overflow and a licence dialog all end the same way:
//!   the child dies, the module goes on the persistent block list, and this
//!   process never noticed.
//!
//! That last point is not theoretical. Before it, the *first* scan on the
//! development machine killed MYDAW outright -- before the block list had learned
//! which module did it, so the next launch did exactly the same thing.
//!
//! The old in-process path ([`scan::scan_module`]: one worker thread, one
//! deadline) survives as the fallback for a build with no worker executable, and
//! it is still worse in the way that matters: a module that hangs inside
//! `DllMain` holds the process-wide loader lock, so every module after it is
//! unopenable too. `examples/scan_report.rs --in-process` measures exactly that.

pub mod blocklist;
pub mod gui;
pub mod host;
pub mod hosting;
pub mod instances;
pub mod module;
mod parameters;
mod preset;
pub mod scan;
pub mod test_util;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mydaw_kernel::{KernelCtx, PanelManifest, Plugin, PluginManifest};
use mydaw_plugin_scan::{ModuleScan, ScanFailure};
use parking_lot::Mutex;

pub use blocklist::{BlockList, BLOCKLIST_FILE};
pub use gui::{Vst3GuiError, Vst3GuiPump, Vst3GuiSession, FALLBACK_SIZE};
pub use host::{Vst3GuiRequests, Vst3HostRequests, MAX_BLOCK_EVENTS, MAX_STATE_BYTES};
pub use hosting::{
    LoadedVst3Plugin, Vst3BusLayout, Vst3HostError, Vst3Node, Vst3NodeStats, RETIRE_TIMEOUT,
};
pub use instances::{
    Vst3GuiState, Vst3InstanceError, Vst3InstanceHost, Vst3InstanceId, ACTIVATE_WAIT,
};
pub use module::{ClassId, Vst3Module, Vst3ModuleError};
pub use scan::{
    scan_worker_handler, standard_search_paths, Vst3PluginInfo, Vst3PluginKind, Vst3ScanError,
    SCAN_TIMEOUT,
};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "host-vst3";

/// RT node type id this host claims for nodes it builds.
pub const NODE_TYPE_ID: &str = "vst3";

/// Panel id of the VST3 browser, contributed to the webview at registration.
pub const BROWSER_PANEL_ID: &str = "vst3-browser";

/// Frontend component key rendering [`BROWSER_PANEL_ID`]. Mirrored in
/// `ui/src/panels/registry.ts`; renaming it is a breaking UI change.
pub const BROWSER_PANEL_COMPONENT: &str = "vst3-browser";

/// VST3 discovery service.
///
/// Provided as a kernel service by [`HostVst3Plugin`]; resolve it with
/// `ctx.services.get::<Vst3HostService>()`.
pub struct Vst3HostService {
    search_paths: Vec<PathBuf>,
    /// Result of the last [`scan`](Self::scan), if one has happened.
    ///
    /// A scan opens every installed module, which is slow (seconds on a
    /// well-stocked machine, because VST3 modules are large and several here are
    /// over 100 MB) and runs third-party code. Doing it once per process and
    /// serving the cache is the difference between a panel that opens and one
    /// that hangs.
    cache: Mutex<Option<Arc<[Vst3PluginInfo]>>>,
    /// How modules are actually opened: in a child process, against a metadata
    /// cache, minus everything on the block list. See
    /// [`mydaw_plugin_scan::ModuleScan`].
    scanner: ModuleScan<Vst3PluginInfo>,
}

/// What one [`Vst3HostService::scan_detailed`] pass found.
///
/// The plain [`scan`](Vst3HostService::scan) returns only the plugins, because
/// that is all a picker needs. This carries the rest, which is what a diagnostic
/// report and the step 7 compatibility table are made of.
#[derive(Debug, Default)]
pub struct Vst3ScanReport {
    /// Every plugin class found, in module order.
    pub plugins: Vec<Vst3PluginInfo>,
    /// Modules that produced nothing, with the reason.
    pub failures: Vec<(PathBuf, String)>,
    /// Modules this pass added to the block list: each one crashed its scan
    /// worker or never answered. They are not counted as scanned and will not be
    /// opened again until the user clears the list.
    pub blocked: Vec<PathBuf>,
    /// The module that wedged this scan, if one did.
    ///
    /// `Some` means the scan **stopped early**: that module blocked inside
    /// `DllMain` and is holding the process-wide loader lock, so nothing after it
    /// could have been opened.
    ///
    /// Only reachable in the in-process fallback, i.e. a build with no scan
    /// worker executable. A child hanging costs one module and nothing else,
    /// which is the entire point of scanning out of process.
    pub aborted_at: Option<PathBuf>,
    /// Modules not opened: already block-listed, or after
    /// [`aborted_at`](Self::aborted_at) stopped the pass.
    pub skipped: Vec<PathBuf>,
    /// Modules answered from the metadata cache, having opened nothing.
    pub cache_hits: usize,
    /// Child processes this pass started -- one per unknown or changed module.
    pub children: usize,
    /// Modules opened inside *this* process because no worker could be found.
    pub in_process: usize,
    /// How long the whole pass took.
    pub elapsed: std::time::Duration,
}

impl Vst3ScanReport {
    /// Whether the pass stopped early because a module wedged the loader.
    ///
    /// A caller that shows a plugin list should say so: "37 plugins not scanned"
    /// is a very different message from "you have 6 plugins".
    pub fn was_aborted(&self) -> bool {
        self.aborted_at.is_some()
    }

    /// How many modules were not opened at all.
    pub fn not_attempted(&self) -> usize {
        self.skipped.len()
    }

    /// Whether the pass answered entirely from the cache and the block list.
    ///
    /// True for every scan after the first on an unchanged machine, which is the
    /// difference between a browser that opens instantly and one that starts
    /// dozens of processes.
    pub fn opened_nothing(&self) -> bool {
        self.children == 0 && self.in_process == 0
    }
}

impl From<mydaw_plugin_scan::ScanPass<Vst3PluginInfo>> for Vst3ScanReport {
    fn from(pass: mydaw_plugin_scan::ScanPass<Vst3PluginInfo>) -> Self {
        Self {
            plugins: pass.found,
            failures: pass
                .failures
                .into_iter()
                .map(|(path, failure)| {
                    let reason = match &failure {
                        // The shared crate keeps its messages format-neutral;
                        // "VST3 module <path>" is what a VST3 browser wants to
                        // show, and matches what the module errors already say.
                        ScanFailure::Blocked(message) => {
                            format!("VST3 module {}: {message}", path.display())
                        }
                        ScanFailure::Refused(message) => message.clone(),
                    };
                    (path, reason)
                })
                .collect(),
            blocked: pass.newly_blocked,
            aborted_at: pass.aborted_at,
            skipped: pass.skipped,
            cache_hits: pass.cache_hits,
            children: pass.children,
            in_process: pass.in_process,
            elapsed: pass.elapsed,
        }
    }
}

impl Default for Vst3HostService {
    fn default() -> Self {
        Self::new()
    }
}

impl Vst3HostService {
    /// Service scanning the standard locations, with the user's persistent block
    /// list and metadata cache, opening unknown modules in child processes. See
    /// [`standard_search_paths`](scan::standard_search_paths).
    pub fn new() -> Self {
        Self::with_search_paths_and_scan(scan::standard_search_paths(), ModuleScan::persistent())
    }

    /// Service scanning exactly the given roots, remembering nothing between
    /// runs.
    ///
    /// In-memory on purpose: this is the constructor tests use, and a test that
    /// wrote to the developer's real block list or scan cache would be a test
    /// that changed which plugins their DAW can see. Modules are still opened out
    /// of process.
    pub fn with_search_paths(search_paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self::with_search_paths_and_scan(search_paths, ModuleScan::in_memory())
    }

    /// Service scanning the given roots with the scanner the caller chose.
    ///
    /// The constructor a *tool* wants: an override of the search paths should not
    /// also throw away the record of which modules wedge the loader, or the tool
    /// spends a quarter of an hour re-learning it.
    pub fn with_search_paths_and_scan(
        search_paths: impl IntoIterator<Item = PathBuf>,
        scanner: ModuleScan<Vst3PluginInfo>,
    ) -> Self {
        Self {
            search_paths: search_paths.into_iter().collect(),
            cache: Mutex::new(None),
            scanner,
        }
    }

    /// The modules this service refuses to open. See [`BlockList`].
    pub fn blocklist(&self) -> &BlockList {
        self.scanner.blocklist()
    }

    /// How this service opens modules: its block list, its metadata cache and
    /// its worker configuration.
    pub fn scanner(&self) -> &ModuleScan<Vst3PluginInfo> {
        &self.scanner
    }

    /// Roots this service walks.
    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    /// Every `*.vst3` path under the search roots, deduplicated.
    pub fn module_paths(&self) -> Vec<PathBuf> {
        scan::bundle_paths(&self.search_paths)
    }

    /// Find every VST3 plugin under the search paths.
    ///
    /// Never fails: an unreadable root, a module that will not load, one that
    /// panics and one that hangs are all logged and skipped. A scan of a machine
    /// with no VST3 plugins returns an empty vector.
    ///
    /// **Not cheap and not RT-safe.** Call it from a worker thread, once, and
    /// cache the result -- which is what [`cached_scan`](Self::cached_scan) does.
    pub fn scan(&self) -> Vec<Vst3PluginInfo> {
        self.scan_detailed().plugins
    }

    /// [`scan`](Self::scan), plus everything that went wrong.
    ///
    /// A module that crashes its worker or never answers is added to the block
    /// list here, which is the only place that happens: a dead child is the only
    /// evidence a host gets that a module is unopenable or waiting on a human.
    pub fn scan_detailed(&self) -> Vst3ScanReport {
        self.scanner.scan_all(&self.module_paths()).into()
    }

    /// Plugin list, opening nothing.
    ///
    /// Answers from the on-disk cache for modules whose fingerprint still
    /// matches. Unknown or replaced files are skipped until
    /// [`refresh`](Self::refresh). Opening the plugin browser must not run a
    /// vendor `init()` — that is what pops a license window.
    pub fn cached_scan(&self) -> Arc<[Vst3PluginInfo]> {
        if let Some(cached) = self.cache.lock().clone() {
            return cached;
        }
        let scanned: Arc<[Vst3PluginInfo]> =
            self.scanner.list_cached(&self.module_paths()).found.into();
        *self.cache.lock() = Some(Arc::clone(&scanned));
        scanned
    }

    /// Discard the cache and scan again. Returns the fresh list.
    pub fn refresh(&self) -> Arc<[Vst3PluginInfo]> {
        let scanned: Arc<[Vst3PluginInfo]> = self.scan().into();
        *self.cache.lock() = Some(Arc::clone(&scanned));
        scanned
    }

    /// Whether [`cached_scan`](Self::cached_scan) would answer without scanning.
    pub fn has_cached_scan(&self) -> bool {
        self.cache.lock().is_some()
    }

    /// The scanned plugin with this class id, if one was found.
    pub fn find(&self, cid: ClassId) -> Option<Vst3PluginInfo> {
        self.cached_scan()
            .iter()
            .find(|plugin| plugin.cid == cid)
            .cloned()
    }

    /// Load and instantiate the plugin with class id `cid` from `path`.
    ///
    /// Bypasses the scan: this is the path a project file (which stores module
    /// path + class id) takes.
    ///
    /// # Errors
    /// Anything [`LoadedVst3Plugin::load`] reports.
    pub fn load(&self, path: &Path, cid: ClassId) -> Result<LoadedVst3Plugin, Vst3HostError> {
        LoadedVst3Plugin::load(path, cid)
    }
}

impl std::fmt::Debug for Vst3HostService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3HostService")
            .field("search_paths", &self.search_paths)
            .field("scanned", &self.has_cached_scan())
            .finish()
    }
}

/// Provides [`Vst3HostService`]. Scans nothing at boot.
///
/// Discovery loads third-party code and walks the filesystem; doing that during
/// kernel boot would make start-up as slow and as fragile as the worst plugin
/// installed on the machine -- which, on a machine with licence-protected
/// plugins, means a DAW that will not start until somebody dismisses a dialog.
#[derive(Default)]
pub struct HostVst3Plugin {
    service: Option<Arc<Vst3HostService>>,
}

impl HostVst3Plugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// The service this plugin provided, once `register` has run.
    pub fn service(&self) -> Option<&Arc<Vst3HostService>> {
        self.service.as_ref()
    }
}

impl Plugin for HostVst3Plugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, "VST3 Host")
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        let service = Arc::new(Vst3HostService::new());
        ctx.services
            .provide(Arc::clone(&service))
            .expect("Vst3HostService provided twice: host-vst3 registered more than once");
        // The browser is a *view* of the scan, not the scan itself: contributing
        // the panel costs nothing and still does not load a single module.
        ctx.contribute_panel(PanelManifest::new(
            BROWSER_PANEL_ID,
            "VST3 Plugins",
            BROWSER_PANEL_COMPONENT,
        ));
        self.service = Some(service);
    }

    fn start(&mut self, _ctx: &KernelCtx) {}

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mydaw-vst3-svc-{tag}-{}-{unique}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scanning_nothing_at_all_yields_nothing() {
        let missing = std::env::temp_dir().join("mydaw-vst3-definitely-not-here-1a7c");
        let service = Vst3HostService::with_search_paths([missing]);
        assert!(service.scan().is_empty());
        assert!(service.module_paths().is_empty());

        let empty = Vst3HostService::with_search_paths([]);
        assert!(empty.search_paths().is_empty());
        assert!(empty.scan().is_empty());
    }

    #[test]
    fn one_broken_module_does_not_hide_the_rest_of_the_tree() {
        let dir = TempDir::new("mixed");
        std::fs::write(dir.path().join("a.vst3"), b"broken").unwrap();
        std::fs::write(dir.path().join("b.vst3"), b"also broken").unwrap();

        let service = Vst3HostService::with_search_paths([dir.path().to_path_buf()]);
        // Both are walked (neither aborts the walk) and both are skipped.
        assert_eq!(service.module_paths().len(), 2);
        assert!(service.scan().is_empty());
    }

    #[test]
    fn the_scan_cache_is_populated_lazily_and_reused() {
        let dir = TempDir::new("cache");
        let service = Vst3HostService::with_search_paths([dir.path().to_path_buf()]);

        assert!(!service.has_cached_scan(), "nothing is scanned up front");
        let first = service.cached_scan();
        assert!(service.has_cached_scan());
        assert!(
            Arc::ptr_eq(&first, &service.cached_scan()),
            "the second call must serve the cache, not rescan"
        );

        // A module appearing after the scan stays invisible until a refresh.
        std::fs::write(dir.path().join("late.vst3"), b"broken").unwrap();
        assert!(Arc::ptr_eq(&service.cached_scan(), &first));
        assert!(!Arc::ptr_eq(&service.refresh(), &first));
    }

    #[test]
    fn looking_up_a_class_id_that_was_never_scanned_finds_nothing() {
        let dir = TempDir::new("find");
        let service = Vst3HostService::with_search_paths([dir.path().to_path_buf()]);
        assert!(service.find(ClassId([1; 16])).is_none());
    }

    #[test]
    fn loading_a_module_that_is_not_a_library_is_a_typed_error() {
        let dir = TempDir::new("not-a-library");
        let bogus = dir.path().join("garbage.vst3");
        std::fs::write(&bogus, b"this is not a shared library").unwrap();

        let service = Vst3HostService::with_search_paths([dir.path().to_path_buf()]);
        let err = service
            .load(&bogus, ClassId([0; 16]))
            .expect_err("a text file is not a VST3 module");
        assert!(matches!(err, Vst3HostError::Module(_)), "got {err:?}");
    }

    #[test]
    fn the_service_is_registered_and_scans_nothing_at_boot() {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(HostVst3Plugin::new())).unwrap();
        kernel.boot().unwrap();

        assert_eq!(kernel.boot_order(), vec![PLUGIN_ID]);
        let service = kernel
            .ctx()
            .services
            .get::<Vst3HostService>()
            .expect("service should be provided");
        assert!(!service.search_paths().is_empty());
        assert!(
            !service.has_cached_scan(),
            "registration must not open a single module"
        );

        // One panel, and no RT node factory: a VST3 node is built from a chosen
        // module + class id, which a stateless `RtNodeFactory` cannot express.
        let panels = kernel.ui_panels();
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0].id, BROWSER_PANEL_ID);
        assert_eq!(panels[0].title, "VST3 Plugins");
        assert_eq!(panels[0].component, BROWSER_PANEL_COMPONENT);
        assert!(kernel.ctx().rt_node_factory(NODE_TYPE_ID).is_none());
    }
}
