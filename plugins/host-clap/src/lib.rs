//! `mydaw-plugin-host-clap` -- CLAP plugin discovery.
//!
//! **Phase 0 scope is discovery only.** This crate finds `.clap` bundles, opens
//! each one far enough to read its plugin descriptors, and reports what it found.
//! It instantiates nothing and processes no audio: that is Phase 1 spike 2
//! ("load real CLAP plugin, process audio through it"), which will wrap a plugin
//! instance in an [`RtNode`](mydaw_kernel::RtNode) behind the same seam the VST3
//! host will use.
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

use std::ffi::CStr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clack_host::entry::PluginEntry;
use clack_host::plugin::PluginDescriptor;
use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "host-clap";

/// RT node type id this host will claim once it can build nodes (spike 2).
pub const NODE_TYPE_ID: &str = "clap";

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
    fn from_descriptor(descriptor: &PluginDescriptor, path: &Path) -> Option<Self> {
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
        }
    }

    /// Service scanning exactly the given roots. Missing roots are ignored.
    pub fn with_search_paths(search_paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            search_paths: search_paths.into_iter().collect(),
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
        assert!(kernel.ui_panels().is_empty());
        assert!(kernel.ctx().rt_node_factory(NODE_TYPE_ID).is_none());
    }
}
