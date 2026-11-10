//! Finding VST3 effects without betting the DAW on them.
//!
//! Reading a module's class list runs the vendor's code, which can crash the
//! process, or *hang* it: an unlicensed plugin may put up an activation dialog
//! and never return, and on Windows a module blocked inside `DllMain` holds the
//! per-process loader lock, so every later module load hangs too. (Measured on
//! the old host's development machine: an in-process scan found 1 of 29
//! plugins in 420 s; one child process per module found all 29.)
//!
//! So modules are opened by [`wave_plugin_scan::ModuleScan`]: one **child
//! process** per unknown module with a deadline, a metadata **cache** keyed by
//! the binary's size + mtime (an unchanged machine opens nothing), and a
//! persistent **block list** for modules that crashed or hung their child.
//! This file supplies the VST3 half: how to read one module
//! ([`ScannedClass::read_module`]), which file to fingerprint (the binary
//! inside a bundle, not the directory), and the walk over the search paths.
//!
//! The child is normally the app binary itself, re-executed with the scan
//! request in its environment; see [`run_scan_worker_if_requested`].

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use vst3::ComPtr;
use vst3::Steinberg::{
    kResultOk, IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactoryTrait,
    PClassInfo, PClassInfo2,
};
use wave_plugin_scan::{ModuleScan, ScanTarget, WorkerSpawn};

use crate::module::{c_string_to_string, has_vst3_extension, resolve_binary, ClassId, Module};

/// The `category` of a class a host can instantiate as a plugin (a `#define`
/// in the headers, so not in the generated bindings).
pub(crate) const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// How deep to recurse under a search root (vendors nest one level; the limit
/// only bounds a pathological tree or a symlink loop).
const MAX_SCAN_DEPTH: usize = 8;

/// Name of the optional dedicated scan worker this crate ships.
pub const SCAN_WORKER_BIN: &str = "wave-vst3-scan-worker";

/// One VST3 effect, as the plugin browser lists it and a project stores it.
///
/// `(path, cid)` identifies a plugin: a module can export several classes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    /// The `.vst3` bundle (or flat file) the class lives in.
    pub path: String,
    /// The class id, 32 uppercase hex digits.
    pub cid: String,
    pub name: String,
    /// Empty for a module that implements only `IPluginFactory`.
    pub vendor: String,
    /// VST3's `subCategories`, verbatim (`"Fx|Delay"`); empty when undeclared.
    pub category: String,
}

/// One class found in a module: what crosses the worker pipe and lands in the
/// scan cache. The field names match the old host's cache entries, so an
/// existing cache is reused rather than rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ScannedClass {
    pub(crate) cid: ClassId,
    pub(crate) name: String,
    pub(crate) vendor: String,
    pub(crate) version: String,
    pub(crate) subcategories: String,
    /// Never serialised: the parent knows which module it asked about, and a
    /// path is the one field a child running a hostile plugin should not be
    /// trusted to invent. Stamped back on by `with_module_path`.
    #[serde(skip)]
    pub(crate) path: PathBuf,
}

/// What a scanned class advertises itself as, from `subCategories`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Instrument,
    Effect,
    /// Declares subcategories, none of them `Instrument` or `Fx`.
    Other,
    /// Declares none at all (a module with only `IPluginFactory`).
    Unknown,
}

impl Kind {
    /// `Instrument` wins over `Fx` when a plugin declares both.
    pub(crate) fn from_subcategories(subcategories: &str) -> Self {
        let parts: Vec<&str> = subcategories
            .split('|')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        if parts.is_empty() {
            Self::Unknown
        } else if parts
            .iter()
            .any(|part| part.eq_ignore_ascii_case("Instrument"))
        {
            Self::Instrument
        } else if parts.iter().any(|part| part.eq_ignore_ascii_case("Fx")) {
            Self::Effect
        } else {
            Self::Other
        }
    }
}

impl ScannedClass {
    /// Offered as an insert effect: declared `Fx`, or declared nothing (hiding
    /// those would hide working effects; the stereo bus check at
    /// [`instantiate`](crate::instantiate) has the final say).
    pub(crate) fn is_effect(&self) -> bool {
        matches!(
            Kind::from_subcategories(&self.subcategories),
            Kind::Effect | Kind::Unknown
        )
    }

    pub(crate) fn to_info(&self) -> PluginInfo {
        PluginInfo {
            path: self.path.to_string_lossy().into_owned(),
            cid: self.cid.to_hex(),
            name: self.name.clone(),
            vendor: self.vendor.clone(),
            category: self.subcategories.clone(),
        }
    }
}

impl ScanTarget for ScannedClass {
    const FORMAT: &'static str = "vst3";

    fn read_module(module: &Path) -> Result<Vec<Self>, String> {
        // Plugins touch COM during load; in the worker this is the one place
        // that runs.
        crate::module::enter_com_apartment();
        let opened = Module::open(module).map_err(|err| err.to_string())?;
        let classes = read_classes(opened.factory(), module);
        if classes.is_empty() {
            return Err(format!(
                "VST3 module {} exports no audio plugin class",
                module.display()
            ));
        }
        Ok(classes)
    }

    /// The binary, not the bundle directory, whose own metadata says nothing
    /// about the library two levels down.
    fn fingerprint_path(module: &Path) -> PathBuf {
        resolve_binary(module).unwrap_or_else(|_| module.to_path_buf())
    }

    fn with_module_path(mut self, module: &Path) -> Self {
        self.path = module.to_path_buf();
        self
    }
}

/// Enumerate the factory's `"Audio Module Class"` entries. `IPluginFactory2`
/// first: it alone carries vendor, version and `subCategories`.
pub(crate) fn read_classes(factory: &ComPtr<IPluginFactory>, path: &Path) -> Vec<ScannedClass> {
    // SAFETY: `factory` is a live factory from this module; every call uses the
    // declared arity and every out-parameter is an initialised local.
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
                    if c_string_to_string(&info.category) == AUDIO_MODULE_CLASS {
                        found.push(ScannedClass {
                            cid: ClassId::from_tuid(&info.cid),
                            name: c_string_to_string(&info.name),
                            vendor: c_string_to_string(&info.vendor),
                            version: c_string_to_string(&info.version),
                            subcategories: c_string_to_string(&info.subCategories),
                            path: path.to_path_buf(),
                        });
                    }
                    continue;
                }
            }
            let mut info: PClassInfo = std::mem::zeroed();
            if factory.getClassInfo(index, &mut info) == kResultOk
                && c_string_to_string(&info.category) == AUDIO_MODULE_CLASS
            {
                found.push(ScannedClass {
                    cid: ClassId::from_tuid(&info.cid),
                    name: c_string_to_string(&info.name),
                    vendor: String::new(),
                    version: String::new(),
                    subcategories: String::new(),
                    path: path.to_path_buf(),
                });
            }
        }
        found
    }
}

/// The standard VST3 install locations for this platform, plus `%VST3_PATH%`
/// (a path list) for pointing WAVE at a build directory.
///
/// Windows: `C:\Program Files\Common Files\VST3`, `%COMMONPROGRAMFILES%\VST3`,
/// `%LOCALAPPDATA%\Programs\Common\VST3`. Linux: `/usr/lib/vst3`,
/// `/usr/local/lib/vst3`, `~/.vst3`. macOS: `/Library/Audio/Plug-Ins/VST3` and
/// the per-user one (listed for completeness; this build cannot load them).
/// Duplicates are harmless: the walk dedups.
pub fn default_search_paths() -> Vec<PathBuf> {
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
        paths.extend(std::env::split_paths(&extra).filter(|path| !path.as_os_str().is_empty()));
    }
    paths
}

/// Every `*.vst3` entry under `roots`, sorted and deduplicated. A `.vst3`
/// entry is never descended into: a bundle's insides are not more plugins.
pub(crate) fn module_paths(roots: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > MAX_SCAN_DEPTH {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return; // missing or unreadable root: normal
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if has_vst3_extension(&path) {
                out.push(path);
            } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                walk(&path, depth + 1, out);
            }
        }
    }
    let mut paths = Vec::new();
    for root in roots {
        walk(root, 0, &mut paths);
    }
    paths.sort();
    paths.dedup();
    paths
}

/// How the scanner starts a child: the dedicated worker when it sits next to
/// the executable (or one level up from Cargo's `deps/`), otherwise
/// [`WorkerSpawn::Auto`] -- which honours `WAVE_SCAN_WORKER_BIN`, then falls
/// back to the current executable if it called
/// [`run_scan_worker_if_requested`], then to an in-process read with a deadline.
fn worker_spawn() -> WorkerSpawn {
    if std::env::var_os(wave_plugin_scan::worker::WORKER_BIN_ENV).is_some_and(|v| !v.is_empty()) {
        return WorkerSpawn::Auto;
    }
    dedicated_worker().map_or(WorkerSpawn::Auto, WorkerSpawn::Program)
}

fn dedicated_worker() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = format!("{SCAN_WORKER_BIN}{}", std::env::consts::EXE_SUFFIX);
    let sibling = dir.join(&name);
    if sibling.is_file() {
        return Some(sibling);
    }
    if dir
        .file_name()
        .is_some_and(|leaf| leaf == "deps" || leaf == "examples")
    {
        let up = dir.parent()?.join(&name);
        if up.is_file() {
            return Some(up);
        }
    }
    None
}

/// Run one pass over `roots` and turn it into a browser list.
pub(crate) fn scan_with(
    scanner: &ModuleScan<ScannedClass>,
    roots: &[PathBuf],
    rescan: bool,
) -> Vec<PluginInfo> {
    let modules = module_paths(roots);
    let pass = if rescan {
        scanner.scan_all(&modules)
    } else {
        scanner.list_cached(&modules)
    };
    if rescan {
        log::info!(
            "VST3 scan: {} module(s), {} class(es), {} from cache, {} child process(es), {} failure(s), {} newly block-listed",
            modules.len(),
            pass.found.len(),
            pass.cache_hits,
            pass.children,
            pass.failures.len(),
            pass.newly_blocked.len()
        );
        for (module, failure) in &pass.failures {
            log::debug!("VST3 module {}: {failure}", module.display());
        }
    }
    let mut list: Vec<PluginInfo> = pass
        .found
        .iter()
        .filter(|class| class.is_effect())
        .map(ScannedClass::to_info)
        .collect();
    list.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.vendor.cmp(&b.vendor))
            .then_with(|| a.path.cmp(&b.path))
    });
    list.dedup();
    list
}

/// The installed VST3 **effects** (instruments are filtered out), sorted by
/// name. Blocking and slow on a cold rescan: call it off the UI thread.
///
/// - `rescan == false`: the list from memory, or -- first call in this process
///   -- from the on-disk scan cache. **Opens no module**: listing must not run
///   a vendor's `init()`, which is what pops licence windows. On a machine
///   never scanned before, this is empty until the first rescan.
/// - `rescan == true`: opens every new or changed module in a child process
///   (unchanged ones still come from the cache; block-listed ones are
///   skipped), updates cache and block list, and returns the fresh list.
///
/// Never fails: unreadable roots and broken modules are logged and skipped.
pub fn scan(rescan: bool) -> Vec<PluginInfo> {
    static SCANNER: OnceLock<ModuleScan<ScannedClass>> = OnceLock::new();
    static LIST: Mutex<Option<Vec<PluginInfo>>> = Mutex::new(None);
    if !rescan {
        if let Some(list) = LIST.lock().clone() {
            return list;
        }
    }
    let scanner = SCANNER.get_or_init(|| ModuleScan::persistent().with_spawn(worker_spawn()));
    let list = scan_with(scanner, &default_search_paths(), rescan);
    *LIST.lock() = Some(list.clone());
    list
}

/// **Call this first thing in the app's `main()`**, before any window, audio
/// device or plugin exists:
///
/// ```ignore
/// fn main() {
///     if wave_vst3::run_scan_worker_if_requested() {
///         return;
///     }
///     // ... start Tauri
/// }
/// ```
///
/// A scan re-executes the app binary as a sacrificial worker, with the request
/// in its *environment* (`WAVE_SCAN_WORKER=vst3`, `WAVE_SCAN_MODULE=<path>`;
/// the variable names belong to `wave-plugin-scan`). Environment rather than
/// argv, so it cannot collide with Tauri's own arguments or a file dropped on
/// the icon. In that case this scans the one module, prints the result line on
/// stdout and **exits the process**: a process that has loaded a stranger's
/// library must not go on to start the DAW.
///
/// Otherwise it records that this executable can serve as its own worker and
/// returns `false`. (`true` is not observable today because the worker exits;
/// the `bool` keeps the call site correct if that ever changes.)
pub fn run_scan_worker_if_requested() -> bool {
    if wave_plugin_scan::worker::worker_request().is_none() {
        wave_plugin_scan::declare_worker_capable();
        return false;
    }
    wave_plugin_scan::run_worker_if_requested(&[ScannedClass::worker_handler()]);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::scratch_dir;

    fn class(subcategories: &str) -> ScannedClass {
        ScannedClass {
            cid: ClassId([7; 16]),
            name: "Thing".to_owned(),
            vendor: "Acme".to_owned(),
            version: "1.0".to_owned(),
            subcategories: subcategories.to_owned(),
            path: PathBuf::from("/plugins/Thing.vst3"),
        }
    }

    #[test]
    fn subcategories_decide_what_is_offered_as_an_insert_effect() {
        assert_eq!(
            Kind::from_subcategories("Instrument|Synth"),
            Kind::Instrument
        );
        assert_eq!(Kind::from_subcategories("Fx|Reverb"), Kind::Effect);
        assert_eq!(
            Kind::from_subcategories("Fx|Instrument|Pitch Shift"),
            Kind::Instrument
        );
        assert_eq!(Kind::from_subcategories(" FX | Delay "), Kind::Effect);
        assert_eq!(Kind::from_subcategories("Analyzer|Spectrum"), Kind::Other);
        assert_eq!(Kind::from_subcategories("  "), Kind::Unknown);
        assert_eq!(Kind::from_subcategories("Fx||Delay|"), Kind::Effect);

        assert!(class("Fx|Reverb").is_effect());
        assert!(
            class("").is_effect(),
            "an unclassified plugin is still worth offering"
        );
        assert!(!class("Instrument|Synth").is_effect());
        assert!(!class("Analyzer").is_effect());
    }

    #[test]
    fn the_browser_shape_is_camel_case_with_a_hex_class_id() {
        let info = class("Fx|Delay").to_info();
        assert_eq!(info.cid, "07".repeat(16));
        assert_eq!(info.category, "Fx|Delay");
        let json = serde_json::to_value(&info).unwrap();
        for key in ["path", "cid", "name", "vendor", "category"] {
            assert!(json.get(key).is_some(), "{key} missing from {json}");
        }
    }

    #[test]
    fn the_cache_shape_matches_the_old_hosts_entries_and_omits_the_path() {
        let json = serde_json::to_value(class("Fx")).unwrap();
        assert!(
            json.get("path").is_none(),
            "a child must not supply the module path"
        );
        assert_eq!(json["cid"], "07".repeat(16));
        // An entry written by the old host's cache still parses.
        let old = serde_json::json!({
            "cid": "0123456789ABCDEFFEDCBA9876543210",
            "name": "AVOX ASPIRE", "vendor": "Antares", "version": "4.1.0",
            "subcategories": "Fx"
        });
        let parsed: ScannedClass = serde_json::from_value(old).unwrap();
        assert_eq!(parsed.name, "AVOX ASPIRE");
        let stamped = parsed.with_module_path(Path::new("/x/Aspire.vst3"));
        assert_eq!(stamped.path, PathBuf::from("/x/Aspire.vst3"));
    }

    #[test]
    fn modules_are_found_recursively_by_extension_deduplicated_and_never_descended() {
        let dir = scratch_dir("walk");
        let nested = dir.join("Vendor");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("top.vst3"), b"x").unwrap();
        std::fs::write(nested.join("deep.VST3"), b"x").unwrap();
        std::fs::write(dir.join("readme.txt"), b"x").unwrap();
        let bundle = dir.join("Bundle.vst3");
        let arch = bundle.join("Contents").join(crate::module::ARCH_DIR);
        std::fs::create_dir_all(&arch).unwrap();
        std::fs::write(arch.join("Bundle.vst3"), b"x").unwrap();

        let found = module_paths(&[dir.clone(), dir.clone()]);
        assert_eq!(found.len(), 3, "found: {found:?}");
        assert!(found.contains(&bundle), "the bundle itself is the module");
        assert!(module_paths(&[dir.join("missing")]).is_empty());
        assert!(module_paths(&[]).is_empty());
    }

    #[test]
    fn the_default_search_paths_are_vst3_directories() {
        let paths = default_search_paths();
        assert!(!paths.is_empty());
        if std::env::var_os("VST3_PATH").is_none() {
            assert!(
                paths.iter().all(|path| path.ends_with("VST3")
                    || path.ends_with("vst3")
                    || path.ends_with(".vst3")),
                "{paths:?}"
            );
        }
    }

    #[test]
    fn a_broken_module_yields_nothing_and_does_not_stop_the_walk() {
        let dir = scratch_dir("broken");
        std::fs::write(dir.join("a.vst3"), b"not a library").unwrap();
        std::fs::write(dir.join("b.vst3"), b"not one either").unwrap();
        // In-process, so this unit test needs no worker binary; the
        // integration tests cover the child-process path.
        let scanner = ModuleScan::<ScannedClass>::in_memory().with_spawn(WorkerSpawn::InProcess);
        assert!(
            scan_with(&scanner, std::slice::from_ref(&dir), false).is_empty(),
            "listing opens nothing"
        );
        assert!(scan_with(&scanner, std::slice::from_ref(&dir), true).is_empty());
        let err = ScannedClass::read_module(&dir.join("a.vst3")).unwrap_err();
        assert!(err.contains("a.vst3"), "{err}");
    }
}
