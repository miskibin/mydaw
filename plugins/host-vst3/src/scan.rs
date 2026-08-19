//! Finding VST3 modules and reading what is inside them.
//!
//! The shape mirrors `mydaw-plugin-host-clap`'s discovery half: walk the standard
//! directories, open each module far enough to read its class list, report what is
//! installed. Two things differ, and both come from the format rather than from
//! taste:
//!
//! - **A module exports classes, not plugins.** A factory lists processors,
//!   controllers, and whatever else a vendor registered; only the ones whose
//!   category is [`AUDIO_MODULE_CLASS`] are plugins a host can instantiate.
//! - **Classification is a string, not a list.** VST3's `subCategories` field is
//!   one `|`-separated string (`"Instrument|Synth"`, `"Fx|Pitch Shift"`), read
//!   from `IPluginFactory2`. A module that only implements `IPluginFactory` has
//!   no subcategories at all, and [`Vst3PluginKind::Unknown`] is the honest answer
//!   for it rather than a guess.
//!
//! # Why a module is opened in a process MYDAW is willing to lose
//!
//! Opening a module runs a vendor's initialisation code, and on this machine
//! several of the installed plugins are licence-protected: an unlicensed one may
//! put up a modal dialog and never return. `catch_unwind` does not help with that
//! -- nothing has panicked, the call simply has not come back -- and it does not
//! help with an access violation either.
//!
//! # The loader lock, and why a deadline alone is not enough
//!
//! Measured on the development machine (see the step 7 spec): a module that
//! blocks *inside `DllMain`* holds the Windows **loader lock** while it does so,
//! and the loader lock is per-process. Every later `LoadLibraryW` in that process
//! -- for any DLL, from any thread -- then blocks behind it forever.
//!
//! So one wedged plugin does not cost one plugin. It costs **every module after
//! it**, and a scan that carried on regardless would report 42 perfectly good
//! plugins as broken.
//!
//! So [`Vst3HostService::scan_detailed`](crate::Vst3HostService::scan_detailed)
//! does not open modules in this process at all. It hands the list to
//! [`ModuleScan`](mydaw_plugin_scan::ModuleScan), which opens each unknown module
//! in a **child process** and kills it at the deadline: a poisoned loader lock
//! and an access violation both die with the child, and the module goes on the
//! block list rather than taking the DAW with it.
//!
//! [`scan_module_in_process`] is the pure path-in/results-out function that child
//! calls, and the [`ScanTarget`] impl at the bottom of this file is the whole of
//! what this crate has to tell the shared scanner about VST3.
//!
//! [`scan_module`] -- one worker thread, one deadline, everything still in this
//! process -- is what a scan used to do, and is kept because it is the shape of
//! the fallback [`ModuleScan`](mydaw_plugin_scan::ModuleScan) uses when no worker
//! executable can be found, and because `examples/scan_report.rs --in-process`
//! measures the cost of that fallback rather than arguing about it.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use mydaw_plugin_scan::{ScanTarget, WorkerHandler};
use serde::{Deserialize, Serialize};
use vst3::ComPtr;
use vst3::Steinberg::kResultOk;
use vst3::Steinberg::{IPluginFactory, PClassInfo2};
use vst3::Steinberg::{IPluginFactory2, IPluginFactory2Trait, IPluginFactoryTrait, PClassInfo};

use crate::module::{
    c_string_to_string, has_vst3_extension, ClassId, Vst3Module, Vst3ModuleError, VST3_EXTENSION,
};

/// The `category` of a class a host can instantiate as a plugin.
///
/// Spelled out rather than imported: the VST3 headers define it as a `#define`,
/// which the generated bindings do not carry.
pub const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// How deep to recurse under a search root.
///
/// Vendors nest one level (`VST3/Antares/Thing.vst3`). The limit bounds the walk
/// on a pathological tree or a symlink loop, not because deeper layouts mean
/// anything.
pub const MAX_SCAN_DEPTH: usize = 8;

/// How long one module gets to load and list its classes.
///
/// Generous, because a large sample-based plugin genuinely takes seconds to
/// initialise; finite, because an unlicensed one takes forever. See the module
/// docs for what happens at the deadline.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// What a scanned VST3 plugin advertises itself as.
///
/// Read from `subCategories`, exactly as [`ClapPluginKind`] is read from CLAP's
/// `features`: cheap, already scanned, and only the plugin's own claim. The
/// authoritative answer is its bus layout, and that costs an instantiation --
/// see [`Vst3PortLayout`](crate::Vst3PortLayout).
///
/// [`ClapPluginKind`]: https://docs.rs/mydaw-plugin-host-clap
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Vst3PluginKind {
    /// `Instrument`: generates audio from notes. Can head a track.
    Instrument,
    /// `Fx`: processes audio. Can sit in an insert slot.
    AudioEffect,
    /// The plugin declares subcategories, but none this host recognises.
    Other,
    /// The plugin declares no subcategories at all -- which is what a module
    /// implementing only `IPluginFactory` (no `IPluginFactory2`) reports. Not a
    /// failure, and deliberately not guessed at.
    Unknown,
}

impl Vst3PluginKind {
    /// VST3's `Instrument` subcategory.
    pub const INSTRUMENT: &'static str = "Instrument";
    /// VST3's `Fx` subcategory.
    pub const FX: &'static str = "Fx";

    /// Classify a `|`-separated `subCategories` string.
    ///
    /// Instrument wins over `Fx` when a plugin declares both (some vocal
    /// processors do), for the same reason the CLAP host prefers it: a plugin
    /// that can be either is one a user reaches for as an instrument, and track
    /// sources are the scarcer slot.
    pub fn from_subcategories(subcategories: &str) -> Self {
        if subcategories.trim().is_empty() {
            return Self::Unknown;
        }
        let has = |wanted: &str| {
            split_subcategories(subcategories)
                .into_iter()
                .any(|part| part.eq_ignore_ascii_case(wanted))
        };
        if has(Self::INSTRUMENT) {
            Self::Instrument
        } else if has(Self::FX) {
            Self::AudioEffect
        } else {
            Self::Other
        }
    }

    /// Stable string id, as it crosses the IPC boundary.
    ///
    /// Shares its vocabulary with the CLAP host's `kind` so one UI list can badge
    /// both formats: `instrument` and `audio-effect` mean the same thing on
    /// either side.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Instrument => "instrument",
            Self::AudioEffect => "audio-effect",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }
}

/// Split a `subCategories` string into its parts, dropping empties.
pub fn split_subcategories(subcategories: &str) -> Vec<&str> {
    subcategories
        .split('|')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// One plugin class found inside a VST3 module.
///
/// A module can export several, so `path` is not unique across results; the pair
/// `(path, cid)` is, and that pair is what a project file records.
///
/// Serialisable because a scan result crosses a pipe from a worker process and
/// lands in the scan cache. `path` is deliberately **not** part of that shape --
/// see the field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Vst3PluginInfo {
    /// The class id, as hex. Stable across versions -- it is what a project file
    /// stores and what `createInstance` is called with.
    pub cid: ClassId,
    /// Display name.
    pub name: String,
    /// Vendor name; empty for a module exposing only `IPluginFactory`, which
    /// carries no vendor per class.
    pub vendor: String,
    /// Version string; empty for the same reason.
    pub version: String,
    /// The `subCategories` string, verbatim (`"Fx|Pitch Shift"`).
    ///
    /// Kept whole rather than parsed, for the same reason the CLAP host keeps
    /// `features` verbatim: the vocabulary is open-ended, and a browser that
    /// dropped what it did not recognise would be worse for no benefit.
    /// [`kind`](Self::kind) is the interpretation.
    pub subcategories: String,
    /// The `.vst3` module this class came from.
    ///
    /// Never serialised. A worker process is told which module to open and a
    /// cache entry is keyed by it, so the parent already knows the answer -- and
    /// the path is the one field a child that may be running a hostile plugin
    /// should not be trusted to invent. It is stamped back on by
    /// [`ScanTarget::with_module_path`].
    #[serde(skip)]
    pub path: PathBuf,
}

impl Vst3PluginInfo {
    /// What this plugin is *for*, as far as a picker is concerned.
    pub fn kind(&self) -> Vst3PluginKind {
        Vst3PluginKind::from_subcategories(&self.subcategories)
    }

    /// Whether this plugin can head a track.
    pub fn is_instrument(&self) -> bool {
        self.kind() == Vst3PluginKind::Instrument
    }

    /// Whether this plugin can sit in an insert slot.
    ///
    /// [`Vst3PluginKind::Unknown`] counts: a module with no `IPluginFactory2`
    /// declares no subcategories, and refusing to offer it anywhere would hide a
    /// perfectly good effect. The bus layout check at instantiation is what
    /// actually decides.
    pub fn is_audio_effect(&self) -> bool {
        matches!(
            self.kind(),
            Vst3PluginKind::AudioEffect | Vst3PluginKind::Unknown
        )
    }

    /// The subcategories as separate strings, for a browser.
    pub fn subcategory_list(&self) -> Vec<String> {
        split_subcategories(&self.subcategories)
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

/// Why one module could not be read. Never fatal to a scan.
#[derive(Debug, thiserror::Error)]
pub enum Vst3ScanError {
    /// The module would not open. Carries the loader's own reason.
    #[error(transparent)]
    Module(#[from] Vst3ModuleError),

    /// The module opened but its factory exports no `"Audio Module Class"`.
    /// Legal (a module may export only a controller) and not a malfunction.
    #[error("VST3 module {path} exports no audio plugin class")]
    NoPluginClass { path: PathBuf },

    /// The module did not finish loading within [`SCAN_TIMEOUT`]. Almost always
    /// a licence dialog waiting for a human. See the module docs: the worker
    /// thread and the module it loaded are leaked.
    #[error(
        "VST3 module {path} did not respond within {} s; it is most likely waiting on a \
         licensing dialog",
        SCAN_TIMEOUT.as_secs()
    )]
    TimedOut { path: PathBuf },
}

/// The standard places a VST3 module can be installed.
///
/// Windows, per the VST3 specification's locations document:
/// - `C:\Program Files\Common Files\VST3` (system-wide)
/// - `%COMMONPROGRAMFILES%\VST3` -- normally the same directory, but honours a
///   relocated Program Files. Duplicates are removed by [`bundle_paths`].
/// - `%LOCALAPPDATA%\Programs\Common\VST3` (per-user)
///
/// Plus `%VST3_PATH%`, the conventional override, which is how a developer points
/// the DAW at a build directory.
pub fn standard_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(windows)]
    {
        paths.push(PathBuf::from(r"C:\Program Files\Common Files\VST3"));
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            paths.push(PathBuf::from(common).join("VST3"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Common")
                    .join("VST3"),
            );
        }
    }

    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/VST3"));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        paths.push(PathBuf::from("/usr/lib/vst3"));
        paths.push(PathBuf::from("/usr/local/lib/vst3"));
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".vst3"));
        }
    }

    if let Some(extra) = std::env::var_os("VST3_PATH") {
        paths.extend(std::env::split_paths(&extra));
    }

    paths
}

/// Every `*.vst3` path under `roots`, deduplicated and sorted.
///
/// A `.vst3` entry is never descended into: it is either the DLL itself or a
/// bundle, and a bundle's insides are not more plugins.
pub fn bundle_paths(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for root in roots {
        collect_bundles(root, 0, &mut paths);
    }
    paths.sort();
    paths.dedup();
    paths
}

/// Depth-limited walk collecting `*.vst3` entries into `out`.
pub fn collect_bundles(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Missing or unreadable root: normal on a machine with no VST3 plugins.
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if has_vst3_extension(&path) {
            out.push(path);
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_bundles(&path, depth + 1, out);
        }
    }
}

/// Read the plugin classes out of one module, on a worker thread with a deadline.
///
/// This is the function an out-of-process scanner would call, unchanged: path in,
/// results out, no state.
///
/// # Errors
/// [`Vst3ScanError::TimedOut`] for a module that blocks (see the module docs for
/// what that leaks), plus anything [`Vst3Module::open`] reports.
pub fn scan_module(path: &Path) -> Result<Vec<Vst3PluginInfo>, Vst3ScanError> {
    let owned = path.to_path_buf();
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("mydaw-vst3-scan".to_owned())
        .spawn(move || {
            // A great many plugins touch COM during module load and expect the
            // loading thread to be in an apartment already. See `ComApartment`.
            let _apartment = crate::module::ComApartment::enter();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scan_module_in_process(&owned)
            }));
            // A caller that has already given up has dropped the receiver; a
            // failed send is that, and is not an error here.
            let _ = tx.send(result.map_err(|_| Vst3ModuleError::Panicked {
                path: owned.clone(),
                stage: "listing its classes",
            }));
        });

    match worker {
        Ok(_handle) => match rx.recv_timeout(SCAN_TIMEOUT) {
            Ok(Ok(result)) => result,
            Ok(Err(panicked)) => Err(Vst3ScanError::Module(panicked)),
            Err(_) => Err(Vst3ScanError::TimedOut {
                path: path.to_path_buf(),
            }),
        },
        // No thread available: fall back to scanning inline rather than
        // reporting every module as broken.
        Err(err) => {
            log::warn!("could not start a VST3 scan thread ({err}); scanning inline");
            scan_module_in_process(path)
        }
    }
}

/// [`scan_module`] without the worker thread or the deadline.
///
/// Public because the timeout is a *policy*, and a caller that already runs in a
/// sacrificial process (the out-of-process scanner this crate is heading towards)
/// should not pay for a second thread.
pub fn scan_module_in_process(path: &Path) -> Result<Vec<Vst3PluginInfo>, Vst3ScanError> {
    let module = Vst3Module::open(path)?;
    let classes = read_classes(module.factory(), path);
    if classes.is_empty() {
        return Err(Vst3ScanError::NoPluginClass {
            path: path.to_path_buf(),
        });
    }
    Ok(classes)
}

/// Everything the shared out-of-process scanner needs to know about VST3.
///
/// Four small answers: what to call this format, how to read one module, which
/// file decides whether a cached answer is stale, and where the module path goes
/// once a result has crossed a pipe.
impl ScanTarget for Vst3PluginInfo {
    const FORMAT: &'static str = "vst3";

    fn read_module(module: &Path) -> Result<Vec<Self>, String> {
        // A great many plugins touch COM during module load and expect the
        // loading thread to be in an apartment already. In the worker process
        // this is the *only* place that happens, so it happens here rather than
        // in whatever wrapper called us. See `ComApartment`.
        let _apartment = crate::module::ComApartment::enter();
        scan_module_in_process(module).map_err(|err| err.to_string())
    }

    /// The binary, not the bundle.
    ///
    /// A VST3 bundle is a *directory*; its own length and timestamp say nothing
    /// about the DLL two levels down inside it, so fingerprinting the directory
    /// would happily serve a stale plugin list after an update.
    fn fingerprint_path(module: &Path) -> PathBuf {
        crate::module::resolve_binary(module).unwrap_or_else(|_| module.to_path_buf())
    }

    fn with_module_path(mut self, module: &Path) -> Self {
        self.path = module.to_path_buf();
        self
    }
}

/// This crate's entry in a worker process's dispatch table.
///
/// `main` hands one of these per format to
/// [`run_worker_if_requested`](mydaw_plugin_scan::run_worker_if_requested); see
/// `src/bin/mydaw-vst3-scan-worker.rs` for the smallest possible caller.
pub fn scan_worker_handler() -> WorkerHandler {
    Vst3PluginInfo::worker_handler()
}

/// Enumerate the factory's `"Audio Module Class"` entries.
///
/// `IPluginFactory2` is asked first because it is the only interface that carries
/// vendor, version and -- crucially -- `subCategories`, which is the whole of the
/// instrument/effect classification. A module offering only `IPluginFactory` still
/// yields usable entries; they simply have no kind.
pub(crate) fn read_classes(factory: &ComPtr<IPluginFactory>, path: &Path) -> Vec<Vst3PluginInfo> {
    // SAFETY: `factory` is a live `IPluginFactory` from this module. Every call
    // below is made with the arity and types the VST3 specification declares, and
    // every out-parameter points at a fully initialised local.
    unsafe {
        let count = factory.countClasses();
        if count <= 0 {
            return Vec::new();
        }
        let factory2: Option<ComPtr<IPluginFactory2>> = factory.cast();

        let mut found = Vec::new();
        for index in 0..count {
            if let Some(factory2) = factory2.as_ref() {
                let mut info: PClassInfo2 = std::mem::zeroed();
                if factory2.getClassInfo2(index, &mut info) == kResultOk {
                    if c_string_to_string(&info.category) != AUDIO_MODULE_CLASS {
                        continue;
                    }
                    found.push(Vst3PluginInfo {
                        cid: ClassId::from_tuid(&info.cid),
                        name: c_string_to_string(&info.name),
                        vendor: c_string_to_string(&info.vendor),
                        version: c_string_to_string(&info.version),
                        subcategories: c_string_to_string(&info.subCategories),
                        path: path.to_path_buf(),
                    });
                    continue;
                }
                // `getClassInfo2` refused for this index; fall through to the
                // base interface rather than dropping the class entirely.
            }

            let mut info: PClassInfo = std::mem::zeroed();
            if factory.getClassInfo(index, &mut info) != kResultOk {
                continue;
            }
            if c_string_to_string(&info.category) != AUDIO_MODULE_CLASS {
                continue;
            }
            found.push(Vst3PluginInfo {
                cid: ClassId::from_tuid(&info.cid),
                name: c_string_to_string(&info.name),
                vendor: String::new(),
                version: String::new(),
                subcategories: String::new(),
                path: path.to_path_buf(),
            });
        }
        found
    }
}

/// Extension of a VST3 module, re-exported so callers need only this module.
pub const MODULE_EXTENSION: &str = VST3_EXTENSION;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mydaw-vst3-scan-{tag}-{}-{unique}",
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

    fn info(subcategories: &str) -> Vst3PluginInfo {
        Vst3PluginInfo {
            cid: ClassId([7; 16]),
            name: "Thing".to_owned(),
            vendor: "Acme".to_owned(),
            version: "1.0".to_owned(),
            subcategories: subcategories.to_owned(),
            path: PathBuf::from(r"C:\plugins\Thing.vst3"),
        }
    }

    #[test]
    fn an_instrument_subcategory_makes_a_track_source() {
        let synth = info("Instrument|Synth");
        assert_eq!(synth.kind(), Vst3PluginKind::Instrument);
        assert!(synth.is_instrument());
        assert!(!synth.is_audio_effect());
        assert_eq!(synth.subcategory_list(), vec!["Instrument", "Synth"]);
        assert_eq!(Vst3PluginKind::Instrument.id(), "instrument");
    }

    #[test]
    fn an_fx_subcategory_makes_an_insert_effect() {
        let reverb = info("Fx|Reverb");
        assert_eq!(reverb.kind(), Vst3PluginKind::AudioEffect);
        assert!(reverb.is_audio_effect());
        assert!(!reverb.is_instrument());
        assert_eq!(Vst3PluginKind::AudioEffect.id(), "audio-effect");
    }

    #[test]
    fn a_plugin_declaring_both_is_offered_as_an_instrument() {
        // Some vocal processors do exactly this. Track sources are the scarcer
        // slot, so that is where such a plugin belongs.
        assert_eq!(
            Vst3PluginKind::from_subcategories("Fx|Instrument|Pitch Shift"),
            Vst3PluginKind::Instrument
        );
    }

    #[test]
    fn no_subcategories_at_all_is_unknown_and_still_offered_as_an_effect() {
        // A module implementing only `IPluginFactory` reports nothing here, and
        // hiding those would hide working plugins. The bus-layout check at
        // instantiation is the one that actually refuses.
        let bare = info("");
        assert_eq!(bare.kind(), Vst3PluginKind::Unknown);
        assert!(!bare.is_instrument());
        assert!(
            bare.is_audio_effect(),
            "an unclassified plugin is still worth offering as an insert"
        );
        assert_eq!(
            Vst3PluginKind::from_subcategories("   "),
            Vst3PluginKind::Unknown
        );
        assert!(bare.subcategory_list().is_empty());
    }

    #[test]
    fn an_unrecognised_subcategory_is_other_and_goes_nowhere() {
        let odd = info("Analyzer|Spectrum");
        assert_eq!(odd.kind(), Vst3PluginKind::Other);
        assert!(!odd.is_instrument());
        assert!(
            !odd.is_audio_effect(),
            "a plugin that declared subcategories and chose none we know is not an insert"
        );
    }

    #[test]
    fn subcategory_matching_ignores_case_and_padding() {
        assert_eq!(
            Vst3PluginKind::from_subcategories(" instrument | synth "),
            Vst3PluginKind::Instrument
        );
        assert_eq!(
            Vst3PluginKind::from_subcategories("FX|Delay"),
            Vst3PluginKind::AudioEffect
        );
        assert_eq!(split_subcategories("Fx||Delay|"), vec!["Fx", "Delay"]);
    }

    #[test]
    fn modules_are_found_recursively_and_by_extension_only() {
        let dir = TempDir::new("walk");
        let nested = dir.path().join("Antares");
        std::fs::create_dir_all(&nested).unwrap();

        std::fs::write(dir.path().join("top.vst3"), b"not a module").unwrap();
        std::fs::write(nested.join("deep.VST3"), b"not a module").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"ignore me").unwrap();
        std::fs::write(nested.join("thing.dll"), b"ignore me").unwrap();

        let found = bundle_paths(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 2, "found: {found:?}");
        assert!(found.iter().all(|path| has_vst3_extension(path)));
    }

    #[test]
    fn a_bundle_directory_is_listed_but_never_descended_into() {
        // Its insides are one binary, not more plugins -- and the binary is
        // itself called `.vst3`, so descending would double-count every bundle.
        let dir = TempDir::new("bundle-walk");
        let bundle = dir.path().join("Serum.vst3");
        let arch = bundle.join("Contents").join(crate::module::ARCH_DIRS[0]);
        std::fs::create_dir_all(&arch).unwrap();
        std::fs::write(arch.join("Serum.vst3"), b"not a module").unwrap();

        let found = bundle_paths(&[dir.path().to_path_buf()]);
        assert_eq!(found, vec![bundle]);
    }

    #[test]
    fn overlapping_search_roots_are_deduplicated() {
        // `%COMMONPROGRAMFILES%\VST3` usually *is* `C:\Program Files\Common
        // Files\VST3`, so without this every plugin would be listed twice.
        let dir = TempDir::new("dedupe");
        std::fs::write(dir.path().join("one.vst3"), b"not a module").unwrap();
        let root = dir.path().to_path_buf();
        assert_eq!(bundle_paths(&[root.clone(), root]).len(), 1);
    }

    #[test]
    fn scanning_a_directory_that_is_not_there_yields_nothing() {
        let missing = std::env::temp_dir().join("mydaw-vst3-definitely-not-here-4c81");
        assert!(!missing.exists());
        assert!(bundle_paths(&[missing]).is_empty());
        assert!(bundle_paths(&[]).is_empty());
    }

    #[test]
    fn a_module_that_will_not_load_is_reported_not_fatal() {
        let dir = TempDir::new("garbage");
        let bogus = dir.path().join("garbage.vst3");
        std::fs::write(&bogus, b"this is not a shared library").unwrap();

        let err = scan_module(&bogus).expect_err("a text file is not a VST3 module");
        assert!(matches!(err, Vst3ScanError::Module(_)), "got {err:?}");
        // ...and the walk that found it still succeeds.
        assert_eq!(bundle_paths(&[dir.path().to_path_buf()]).len(), 1);
    }

    #[test]
    fn the_standard_search_paths_all_end_in_a_vst3_directory() {
        let paths = standard_search_paths();
        assert!(!paths.is_empty());
        assert!(
            paths.iter().all(|path| path.ends_with("VST3")
                || path.ends_with("vst3")
                || path.ends_with(".vst3")
                || std::env::var_os("VST3_PATH").is_some()),
            "unexpected default search path in {paths:?}"
        );
    }

    #[test]
    fn the_scan_timeout_is_long_enough_for_a_slow_plugin_and_short_enough_to_notice() {
        // A sample-based instrument really does take seconds to initialise; a
        // plugin waiting on a licence dialog never finishes at all.
        assert!(SCAN_TIMEOUT >= Duration::from_secs(5));
        assert!(SCAN_TIMEOUT <= Duration::from_secs(30));
    }
}
