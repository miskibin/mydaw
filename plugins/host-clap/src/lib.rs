//! `mydaw-plugin-host-clap` -- CLAP plugin discovery and hosting.
//!
//! Two halves:
//!
//! - **Discovery** (this module): find `.clap` bundles, open each far enough to
//!   read its plugin descriptors, report what is installed. Cheap, cached, and
//!   safe to call from a UI command.
//! - **Hosting** ([`hosting`]): instantiate one of those plugins, activate it,
//!   and run it inside the RT graph as a [`ClapNode`] -- Phase 1 spike 2. Read
//!   the [`hosting`] module docs before touching it: CLAP's thread model is not
//!   optional, and the deactivation-on-drop story is subtle.
//!
//! # Crash isolation -- read this before trusting a scan
//! Loading a CLAP bundle runs third-party code inside this process: the OS loader
//! runs the DLL's entry point, then `clap_entry->init()` runs the vendor's own
//! initialisation. That code can do anything, including crash.
//!
//! [`ClapHostService::scan_bundle`] wraps each bundle in
//! [`std::panic::catch_unwind`], so a bundle that panics through an FFI boundary
//! is reported and skipped rather than taking the scan (and the DAW) with it.
//! **That is the limit of what an in-process scanner can do.** A segfault, a
//! stack overflow, an `abort()` or a hijacked exception filter kills the process
//! outright, and no amount of Rust error handling changes that.
//!
//! TODO(phase 2): move scanning out of process -- spawn `mydaw --scan <dir>`, read
//! results over a pipe, treat a non-zero exit as "this bundle is broken" and
//! blacklist it. That is the only design that survives a hostile plugin, and it
//! also gets crash isolation for VST3 for free (see `mydaw-plugin-host-vst3`).
//! The seam is already here: [`ClapHostService::scan_bundle`] is a pure
//! path-in/results-out function, so the subprocess can call exactly it.

pub mod gui;
pub mod hosting;
pub mod test_util;
pub mod timers;

use std::ffi::CStr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clack_host::entry::PluginEntry;
use clack_host::plugin::PluginDescriptor;
use mydaw_kernel::{KernelCtx, PanelManifest, Plugin, PluginManifest};
use parking_lot::Mutex;

pub use gui::{ClapGuiError, ClapGuiPump, ClapGuiSession, ResizeDecision};
pub use hosting::{
    ClapGuiRequests, ClapHostError, ClapNode, ClapNodeStats, ClapPortLayout, LoadedClapPlugin,
    MainThreadWork, RETIRE_TIMEOUT,
};
pub use timers::ClapTimers;

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "host-clap";

/// RT node type id this host claims for nodes it builds.
pub const NODE_TYPE_ID: &str = "clap";

/// Panel id of the CLAP browser, contributed to the webview at registration.
pub const BROWSER_PANEL_ID: &str = "clap-browser";

/// Frontend component key rendering [`BROWSER_PANEL_ID`]. Mirrored in
/// `ui/src/panels/registry.ts`; renaming it is a breaking UI change.
pub const BROWSER_PANEL_COMPONENT: &str = "clap-browser";

/// File extension of a CLAP bundle.
const CLAP_EXTENSION: &str = "clap";

/// How deep to recurse under a search root.
///
/// Vendors nest one or two levels (`CLAP/Vendor/Thing.clap`). The limit exists to
/// bound the walk on a pathological tree or a symlink loop, not because deeper
/// layouts are meaningful.
const MAX_SCAN_DEPTH: usize = 8;

/// A plugin found inside a CLAP bundle.
///
/// One bundle can export several plugins, so `path` is not unique across results;
/// `id` is (it is the plugin's stable CLAP identifier, which is what a project
/// file stores).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClapPluginInfo {
    /// CLAP plugin id, e.g. `"com.u-he.diva"`. Stable across versions.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Vendor name, empty if the bundle does not declare one.
    pub vendor: String,
    /// Version string, empty if the bundle does not declare one.
    pub version: String,
    /// Bundle this plugin came from.
    pub path: PathBuf,
}

impl ClapPluginInfo {
    /// Convert a descriptor. Returns `None` for a descriptor with no id, which is
    /// malformed -- an id is the one field the format requires.
    pub(crate) fn from_descriptor(descriptor: &PluginDescriptor, path: &Path) -> Option<Self> {
        let id = cstr_to_string(descriptor.id())?;
        if id.is_empty() {
            return None;
        }
        Some(Self {
            id,
            name: cstr_to_string(descriptor.name()).unwrap_or_default(),
            vendor: cstr_to_string(descriptor.vendor()).unwrap_or_default(),
            version: cstr_to_string(descriptor.version()).unwrap_or_default(),
            path: path.to_path_buf(),
        })
    }
}

/// Lossy on purpose: a vendor string with invalid UTF-8 should degrade to
/// mojibake in the browser, not make the plugin invisible.
fn cstr_to_string(value: Option<&CStr>) -> Option<String> {
    value.map(|s| s.to_string_lossy().into_owned())
}

/// Why one bundle could not be read. Never fatal to a scan.
#[derive(Debug, thiserror::Error)]
pub enum ClapScanError {
    /// The OS or `clap_entry->init()` refused. Message is preserved as text
    /// because `clack`'s error type is not guaranteed to be `Send + Sync`.
    #[error("could not load CLAP bundle: {0}")]
    Load(String),

    /// The bundle loaded but exports no `clap_plugin_factory` -- not a plugin.
    #[error("bundle exposes no CLAP plugin factory")]
    NoPluginFactory,

    /// The bundle panicked while loading. Caught, reported, skipped.
    #[error("bundle panicked while loading")]
    Panicked,
}

/// CLAP discovery service.
///
/// Provided as a kernel service by [`HostClapPlugin`]; resolve it with
/// `ctx.services.get::<ClapHostService>()`.
pub struct ClapHostService {
    search_paths: Vec<PathBuf>,
    /// Result of the last [`scan`](Self::scan), if one has happened.
    ///
    /// A scan loads every installed CLAP bundle into the process, which is slow
    /// (hundreds of milliseconds on a well-stocked machine) and, worse, runs
    /// third-party code. Doing it once per process and serving the cache is not
    /// an optimisation, it is the difference between a UI panel that opens and
    /// one that hangs. `Arc` so a caller keeps a stable list even if a refresh
    /// replaces the cache underneath.
    cache: Mutex<Option<Arc<[ClapPluginInfo]>>>,
}

impl Default for ClapHostService {
    fn default() -> Self {
        Self::new()
    }
}

impl ClapHostService {
    /// Service scanning the standard locations. See [`standard_search_paths`].
    pub fn new() -> Self {
        Self {
            search_paths: standard_search_paths(),
            cache: Mutex::new(None),
        }
    }

    /// Service scanning exactly the given roots. Missing roots are ignored.
    pub fn with_search_paths(search_paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            search_paths: search_paths.into_iter().collect(),
            cache: Mutex::new(None),
        }
    }

    /// Roots this service walks.
    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    /// Find every CLAP plugin under the search paths.
    ///
    /// Never fails: an unreadable root, a bundle that will not load and a bundle
    /// that panics are all logged and skipped. A scan of a machine with no CLAP
    /// plugins installed returns an empty vector.
    ///
    /// **Not cheap and not RT-safe.** It touches the filesystem and loads
    /// arbitrary shared libraries; call it from a worker thread, once, and cache
    /// the result.
    pub fn scan(&self) -> Vec<ClapPluginInfo> {
        let mut found = Vec::new();
        for bundle in self.bundle_paths() {
            match Self::scan_bundle(&bundle) {
                Ok(plugins) => found.extend(plugins),
                Err(err) => log::warn!("skipping CLAP bundle {}: {err}", bundle.display()),
            }
        }
        found
    }

    /// Every `*.clap` path under the search roots, deduplicated.
    ///
    /// Deduplication matters because the standard roots overlap:
    /// `%COMMONPROGRAMFILES%` usually *is* `C:\Program Files\Common Files`.
    pub fn bundle_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = Vec::new();
        for root in &self.search_paths {
            collect_bundles(root, 0, &mut paths);
        }
        paths.sort();
        paths.dedup();
        paths
    }

    /// Read the descriptors out of one bundle.
    ///
    /// # Panics and crashes
    /// Unwinding panics are caught and reported as [`ClapScanError::Panicked`].
    /// Hard crashes cannot be caught in-process -- see the crate docs.
    pub fn scan_bundle(path: &Path) -> Result<Vec<ClapPluginInfo>, ClapScanError> {
        // `AssertUnwindSafe`: nothing observable is left half-updated if the
        // closure unwinds. It borrows only `path` and builds a fresh `Vec`; the
        // `PluginEntry` it creates is dropped by the unwind itself.
        let result = catch_unwind(AssertUnwindSafe(|| Self::load_descriptors(path)));
        match result {
            Ok(descriptors) => descriptors,
            Err(_) => Err(ClapScanError::Panicked),
        }
    }

    /// Plugin list, scanning at most once per process.
    ///
    /// The first call performs a full [`scan`](Self::scan); later calls return the
    /// same list without touching the filesystem or loading anything. Use
    /// [`refresh`](Self::refresh) after the user installs a plugin.
    pub fn cached_scan(&self) -> Arc<[ClapPluginInfo]> {
        if let Some(cached) = self.cache.lock().clone() {
            return cached;
        }
        // Deliberately scanning outside the lock: a scan runs vendor code and can
        // take a while, and blocking every other caller behind it would turn one
        // slow plugin into a frozen UI. A concurrent double scan is wasteful but
        // harmless -- the last writer wins and both callers get a valid list.
        let scanned: Arc<[ClapPluginInfo]> = self.scan().into();
        *self.cache.lock() = Some(Arc::clone(&scanned));
        scanned
    }

    /// Discard the cache and scan again. Returns the fresh list.
    pub fn refresh(&self) -> Arc<[ClapPluginInfo]> {
        let scanned: Arc<[ClapPluginInfo]> = self.scan().into();
        *self.cache.lock() = Some(Arc::clone(&scanned));
        scanned
    }

    /// Whether [`cached_scan`](Self::cached_scan) would answer without scanning.
    pub fn has_cached_scan(&self) -> bool {
        self.cache.lock().is_some()
    }

    /// Load and instantiate the plugin with CLAP id `id`, found via
    /// [`cached_scan`](Self::cached_scan).
    ///
    /// # Errors
    /// [`ClapHostError::UnknownPluginId`] when nothing scanned has that id, plus
    /// anything [`LoadedClapPlugin::load`] can report.
    pub fn instantiate_by_id(&self, id: &str) -> Result<LoadedClapPlugin, ClapHostError> {
        let plugins = self.cached_scan();
        let found = plugins
            .iter()
            .find(|plugin| plugin.id == id)
            .ok_or_else(|| ClapHostError::UnknownPluginId(id.to_owned()))?;
        LoadedClapPlugin::load(&found.path, id)
    }

    /// Load and instantiate a plugin from an explicit bundle path.
    ///
    /// Bypasses the scan entirely: this is the path a project file (which stores
    /// bundle + id) and the `MYDAW_CLAP_FX` developer override take.
    ///
    /// # Errors
    /// Anything [`LoadedClapPlugin::load`] can report.
    pub fn load(&self, path: &Path, plugin_id: &str) -> Result<LoadedClapPlugin, ClapHostError> {
        LoadedClapPlugin::load(path, plugin_id)
    }

    fn load_descriptors(path: &Path) -> Result<Vec<ClapPluginInfo>, ClapScanError> {
        // SAFETY: `PluginEntry::load` is unsafe because it runs third-party code
        // -- the DLL's entry point and `clap_entry->init()`. There is no way to
        // enumerate a plugin without doing so; the crate docs explain how far the
        // isolation goes. `clack` itself upholds the CLAP entry protocol
        // (init/deinit pairing, refcounting) for the returned value.
        let entry = unsafe { PluginEntry::load(path) }
            .map_err(|err| ClapScanError::Load(err.to_string()))?;

        let factory = entry
            .get_plugin_factory()
            .ok_or(ClapScanError::NoPluginFactory)?;

        Ok(factory
            .plugin_descriptors()
            .filter_map(|descriptor| ClapPluginInfo::from_descriptor(descriptor, path))
            .collect())
    }
}

/// The standard places a CLAP plugin can be installed on this machine.
///
/// Windows, per the CLAP specification:
/// - `C:\Program Files\Common Files\CLAP` (system-wide)
/// - `%COMMONPROGRAMFILES%\CLAP` -- normally the same directory, but honours a
///   relocated Program Files (localised Windows, non-C: installs). Duplicates are
///   removed by [`ClapHostService::bundle_paths`].
/// - `%LOCALAPPDATA%\Programs\Common\CLAP` (per-user)
///
/// Plus `%CLAP_PATH%`, the specification's semicolon-separated override, which is
/// how a developer points the DAW at a build directory.
///
/// Non-Windows targets get the corresponding standard locations so the crate stays
/// cross-platform-clean, as the scaffold spec requires.
pub fn standard_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        paths.push(PathBuf::from(r"C:\Program Files\Common Files\CLAP"));
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            paths.push(PathBuf::from(common).join("CLAP"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Common")
                    .join("CLAP"),
            );
        }
    }

    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/CLAP"));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        paths.push(PathBuf::from("/usr/lib/clap"));
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".clap"));
        }
    }

    // The spec's override. Semicolon-separated on Windows, colon elsewhere --
    // `split_paths` already knows which.
    if let Some(clap_path) = std::env::var_os("CLAP_PATH") {
        paths.extend(std::env::split_paths(&clap_path));
    }

    paths
}

/// Depth-limited walk collecting `*.clap` entries into `out`.
///
/// A `.clap` entry is *not* descended into: on Windows it is a DLL, and on macOS
/// it is a bundle directory whose insides are not more plugins.
fn collect_bundles(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Missing or unreadable root: normal on a machine with no CLAP plugins.
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if has_clap_extension(&path) {
            out.push(path);
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            collect_bundles(&path, depth + 1, out);
        }
    }
}

fn has_clap_extension(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(CLAP_EXTENSION))
}

/// Provides [`ClapHostService`]. Scans nothing at boot.
///
/// Discovery loads third-party code and walks the filesystem; doing that during
/// kernel boot would make start-up as slow and as fragile as the worst plugin
/// installed on the machine. The app calls [`ClapHostService::scan`] when it
/// wants a plugin list.
#[derive(Default)]
pub struct HostClapPlugin {
    service: Option<Arc<ClapHostService>>,
}

impl HostClapPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// The service this plugin provided, once `register` has run.
    pub fn service(&self) -> Option<&Arc<ClapHostService>> {
        self.service.as_ref()
    }
}

impl Plugin for HostClapPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, "CLAP Host")
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        let service = Arc::new(ClapHostService::new());
        ctx.services
            .provide(Arc::clone(&service))
            .expect("ClapHostService provided twice: host-clap registered more than once");
        // The browser is a *view* of the scan, not the scan itself: contributing
        // the panel costs nothing and still does not load a single plugin. The
        // webview asks for the list when the user opens the panel.
        ctx.contribute_panel(PanelManifest::new(
            BROWSER_PANEL_ID,
            "CLAP Plugins",
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
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Self-deleting scratch directory. Avoids a `tempfile` dependency for what
    /// is three lines of `std::fs`.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("mydaw-clap-{tag}-{}-{unique}", std::process::id()));
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
    fn scanning_a_nonexistent_directory_yields_nothing() {
        let missing = std::env::temp_dir().join("mydaw-definitely-not-here-9f2c1a");
        assert!(!missing.exists());

        let service = ClapHostService::with_search_paths([missing]);
        assert!(service.scan().is_empty());
        assert!(service.bundle_paths().is_empty());
    }

    #[test]
    fn scanning_an_empty_directory_yields_nothing() {
        let dir = TempDir::new("empty");
        let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);
        assert!(service.scan().is_empty());
    }

    #[test]
    fn scanning_no_search_paths_at_all_yields_nothing() {
        let service = ClapHostService::with_search_paths([]);
        assert!(service.search_paths().is_empty());
        assert!(service.scan().is_empty());
    }

    #[test]
    fn bundles_are_found_recursively_and_by_extension_only() {
        let dir = TempDir::new("walk");
        let nested = dir.path().join("Vendor").join("Suite");
        std::fs::create_dir_all(&nested).unwrap();

        std::fs::write(dir.path().join("top.clap"), b"not a real bundle").unwrap();
        std::fs::write(nested.join("deep.CLAP"), b"not a real bundle").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"ignore me").unwrap();
        std::fs::write(nested.join("thing.dll"), b"ignore me").unwrap();

        let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);
        let bundles = service.bundle_paths();

        assert_eq!(bundles.len(), 2, "found: {bundles:?}");
        assert!(bundles.iter().all(|p| has_clap_extension(p)));
    }

    #[test]
    fn overlapping_search_roots_are_deduplicated() {
        let dir = TempDir::new("dedupe");
        std::fs::write(dir.path().join("one.clap"), b"not a real bundle").unwrap();

        let service = ClapHostService::with_search_paths([
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        ]);
        assert_eq!(service.bundle_paths().len(), 1);
    }

    #[test]
    fn a_bundle_that_will_not_load_is_reported_not_fatal() {
        let dir = TempDir::new("garbage");
        let bogus = dir.path().join("garbage.clap");
        std::fs::write(&bogus, b"this is not a shared library").unwrap();

        // Reported as an error for this one bundle...
        assert!(matches!(
            ClapHostService::scan_bundle(&bogus),
            Err(ClapScanError::Load(_) | ClapScanError::NoPluginFactory | ClapScanError::Panicked)
        ));

        // ...and simply absent from the scan, which still succeeds.
        let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);
        assert!(service.scan().is_empty());
    }

    #[test]
    fn one_broken_bundle_does_not_hide_the_rest_of_the_tree() {
        let dir = TempDir::new("mixed");
        std::fs::write(dir.path().join("a.clap"), b"broken").unwrap();
        std::fs::write(dir.path().join("b.clap"), b"also broken").unwrap();

        let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);
        // Both are walked (neither aborts the walk) and both are skipped.
        assert_eq!(service.bundle_paths().len(), 2);
        assert!(service.scan().is_empty());
    }

    #[test]
    fn the_standard_search_paths_are_platform_appropriate() {
        let paths = standard_search_paths();
        assert!(!paths.is_empty());
        assert!(
            paths.iter().all(|p| p.ends_with("CLAP")
                || p.ends_with("clap")
                || p.ends_with(".clap")
                || std::env::var_os("CLAP_PATH").is_some()),
            "unexpected default search path in {paths:?}"
        );
    }

    #[test]
    fn the_service_is_registered_and_scans_nothing_at_boot() {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(HostClapPlugin::new())).unwrap();
        kernel.boot().unwrap();

        let service = kernel
            .ctx()
            .services
            .get::<ClapHostService>()
            .expect("service should be provided");
        assert!(!service.search_paths().is_empty());
        assert!(
            !service.has_cached_scan(),
            "registration must not load a single plugin"
        );

        // One panel, and no RT node factory: CLAP nodes are built from a chosen
        // bundle + id, which a stateless `RtNodeFactory` cannot express.
        let panels = kernel.ui_panels();
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0].id, BROWSER_PANEL_ID);
        assert_eq!(panels[0].title, "CLAP Plugins");
        assert_eq!(panels[0].component, BROWSER_PANEL_COMPONENT);
        assert!(kernel.ctx().rt_node_factory(NODE_TYPE_ID).is_none());
    }

    #[test]
    fn the_scan_cache_is_populated_lazily_and_reused() {
        let dir = TempDir::new("cache");
        let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);

        assert!(!service.has_cached_scan(), "nothing is scanned up front");
        let first = service.cached_scan();
        assert!(service.has_cached_scan());
        let second = service.cached_scan();
        assert!(
            Arc::ptr_eq(&first, &second),
            "the second call must serve the cache, not rescan"
        );

        // A bundle appearing after the scan stays invisible until a refresh.
        std::fs::write(dir.path().join("late.clap"), b"broken").unwrap();
        assert!(Arc::ptr_eq(&service.cached_scan(), &first));
        let refreshed = service.refresh();
        assert!(!Arc::ptr_eq(&refreshed, &first));
    }

    #[test]
    fn instantiating_an_unscanned_id_is_a_typed_error() {
        let dir = TempDir::new("unknown-id");
        let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);
        let err = service
            .instantiate_by_id("com.example.not-installed")
            .expect_err("nothing is installed in an empty directory");
        assert!(
            matches!(err, ClapHostError::UnknownPluginId(ref id) if id == "com.example.not-installed"),
            "got {err:?}"
        );
    }

    #[test]
    fn loading_a_bundle_that_is_not_a_library_is_a_typed_error() {
        let dir = TempDir::new("not-a-library");
        let bogus = dir.path().join("garbage.clap");
        std::fs::write(&bogus, b"this is not a shared library").unwrap();

        let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);
        let err = service
            .load(&bogus, "whatever")
            .expect_err("a text file is not a CLAP bundle");
        assert!(
            matches!(err, ClapHostError::BundleLoad { .. }),
            "got {err:?}"
        );
    }
}
