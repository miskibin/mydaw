//! Opening a VST3 module: the shared library, its entry points, its factory.
//!
//! A module is either a flat `.vst3` file (the library itself, legacy Windows
//! layout) or a bundle directory whose binary lives at
//! `Contents/<arch>/<name>` -- [`resolve_binary`] is the rule, unit-tested
//! without a plugin in sight.
//!
//! ```text
//!  load the library            LoadLibraryW / dlopen
//!  InitDll() / ModuleEntry()   optional; false means "refused" (licence checks)
//!  GetPluginFactory()          required: this is what makes it a VST3 module
//! ```
//!
//! # Modules are never unloaded
//!
//! Measured on the old host's development machine: an in-process scan that
//! called `ExitDll` + `FreeLibrary` after each module crashed with an access
//! violation at the eleventh, because two `.vst3` files shared one vendor
//! runtime and freeing the first tore down state the second used. The VST3
//! SDK's own module wrapper caches modules for the life of the process, and so
//! does this one ([`Module::shared`]): a module is loaded once per path and its
//! library is never freed. The cost is address space the OS can page out.

use std::collections::HashMap;
use std::ffi::{c_void, OsStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use vst3::ComPtr;
use vst3::Steinberg::{char8, IPluginFactory, TUID};

use crate::Vst3Error;

/// Sub-directory of a bundle holding the platform binaries.
pub(crate) const CONTENTS_DIR: &str = "Contents";

/// The architecture directory inside a bundle for this build. Only the host's
/// own architecture is loadable.
#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) const ARCH_DIR: &str = "x86_64-win";
#[cfg(all(windows, target_arch = "aarch64"))]
pub(crate) const ARCH_DIR: &str = "arm64-win";
#[cfg(all(windows, target_arch = "x86"))]
pub(crate) const ARCH_DIR: &str = "x86-win";
#[cfg(target_os = "macos")]
pub(crate) const ARCH_DIR: &str = "MacOS";
#[cfg(all(unix, not(target_os = "macos"), target_arch = "x86_64"))]
pub(crate) const ARCH_DIR: &str = "x86_64-linux";
#[cfg(all(unix, not(target_os = "macos"), target_arch = "aarch64"))]
pub(crate) const ARCH_DIR: &str = "aarch64-linux";
#[cfg(not(any(windows, target_os = "macos", all(unix, any(target_arch = "x86_64", target_arch = "aarch64")))))]
pub(crate) const ARCH_DIR: &str = "unsupported";

/// Extension of the binary inside a bundle: `.vst3` on Windows, `.so` on Linux.
#[cfg(windows)]
const BINARY_EXTENSION: &str = "vst3";
#[cfg(not(windows))]
const BINARY_EXTENSION: &str = "so";

/// The 16-byte class id naming one plugin inside a module.
///
/// Stored exactly as the factory reported it and handed back unchanged, so a
/// round trip through a project file cannot re-order bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ClassId(pub(crate) [u8; 16]);

impl ClassId {
    pub(crate) fn from_tuid(tuid: &TUID) -> Self {
        let mut bytes = [0_u8; 16];
        for (out, raw) in bytes.iter_mut().zip(tuid.iter()) {
            *out = *raw as u8;
        }
        Self(bytes)
    }

    pub(crate) fn to_tuid(self) -> TUID {
        let mut tuid: TUID = [0; 16];
        for (out, raw) in tuid.iter_mut().zip(self.0.iter()) {
            *out = *raw as char8;
        }
        tuid
    }

    /// Uppercase hex, 32 characters: what a project file records.
    pub(crate) fn to_hex(self) -> String {
        use std::fmt::Write;
        let mut text = String::with_capacity(32);
        for byte in self.0 {
            let _ = write!(text, "{byte:02X}");
        }
        text
    }

    /// Parse [`to_hex`](Self::to_hex)'s output. Case-insensitive; tolerates the
    /// dashed form vendors print. `None` for anything that is not 32 hex digits.
    pub(crate) fn from_hex(text: &str) -> Option<Self> {
        let digits: Vec<u8> = text
            .chars()
            .filter(|c| *c != '-' && !c.is_whitespace())
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<Vec<u8>>>()?;
        if digits.len() != 32 {
            return None;
        }
        let mut bytes = [0_u8; 16];
        for (index, pair) in digits.as_chunks::<2>().0.iter().enumerate() {
            bytes[index] = (pair[0] << 4) | pair[1];
        }
        Some(Self(bytes))
    }
}

impl std::fmt::Display for ClassId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// As the hex string: it crosses the scan pipe and lands in the scan cache,
/// which humans read when a plugin goes missing.
impl serde::Serialize for ClassId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for ClassId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text).ok_or_else(|| serde::de::Error::custom(format!("{text:?} is not a 32-digit VST3 class id")))
    }
}

/// Turn a `.vst3` path into the path the OS loader is given.
///
/// A flat file *is* the library. A directory is a bundle: the binary is
/// `Contents/<arch>/<bundle stem>.<vst3|so>`, or, failing that, the only
/// binary in the architecture directory (legal, if rare).
pub(crate) fn resolve_binary(path: &Path) -> Result<PathBuf, Vst3Error> {
    if !path.is_dir() {
        // A file, or nothing at all: let the loader say why in its own words.
        return Ok(path.to_path_buf());
    }
    let stem = path.file_stem().unwrap_or_else(|| OsStr::new("plugin"));
    let arch_dir = path.join(CONTENTS_DIR).join(ARCH_DIR);
    let mut expected = arch_dir.join(stem);
    expected.set_extension(BINARY_EXTENSION);
    if expected.is_file() {
        return Ok(expected);
    }
    if let Ok(entries) = std::fs::read_dir(&arch_dir) {
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|candidate| candidate.is_file() && candidate.extension().is_some_and(|ext| ext.eq_ignore_ascii_case(BINARY_EXTENSION)))
            .collect();
        candidates.sort();
        if let Some(only) = candidates.into_iter().next() {
            return Ok(only);
        }
    }
    Err(Vst3Error::NoBinary { path: path.to_path_buf(), looked_in: format!("{CONTENTS_DIR}/{ARCH_DIR}") })
}

/// Whether a path ends in `.vst3`, in any case.
pub(crate) fn has_vst3_extension(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("vst3"))
}

/// A loaded VST3 module and its plugin factory. Lives for the process.
pub(crate) struct Module {
    factory: ComPtr<IPluginFactory>,
    // Never freed; see the module docs. Kept for diagnostics.
    _library: LibraryHandle,
    path: PathBuf,
}

// SAFETY: the library handle is an opaque OS token that is valid on every
// thread, and the factory pointer is `Send + Sync` already. What is
// thread-affine about VST3 (calling the factory) is enforced by running every
// such call on the main thread (see `main_thread`), not by this type.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

impl Module {
    /// The process-wide instance of the module at `path`, opened on first use.
    /// Failures are not cached: a module that failed may be fixed by the user.
    pub(crate) fn shared(path: &Path) -> Result<Arc<Self>, Vst3Error> {
        static MODULES: OnceLock<Mutex<HashMap<PathBuf, Arc<Module>>>> = OnceLock::new();
        let modules = MODULES.get_or_init(Default::default);
        if let Some(module) = modules.lock().get(path) {
            return Ok(Arc::clone(module));
        }
        // Opened without holding the lock: loading runs vendor code, which can
        // take seconds.
        let module = Arc::new(Self::open(path)?);
        Ok(Arc::clone(modules.lock().entry(path.to_path_buf()).or_insert(module)))
    }

    /// Load the module at `path` and take its factory.
    pub(crate) fn open(path: &Path) -> Result<Self, Vst3Error> {
        let binary = resolve_binary(path)?;
        let library = LibraryHandle::load(&binary)?;
        // SAFETY (whole block): `library` is live, and each symbol is called with
        // the signature the VST3 specification declares for it. A module lying
        // about those corrupts the stack, which only the out-of-process scan
        // contains.
        let opened = catch_unwind(AssertUnwindSafe(|| -> Result<_, Vst3Error> {
            if !unsafe { library.enter() } {
                return Err(Vst3Error::InitRefused { path: path.to_path_buf() });
            }
            let get_factory = unsafe { library.symbol::<GetPluginFactoryFn>(b"GetPluginFactory\0") }
                .ok_or_else(|| Vst3Error::NoEntryPoint { path: path.to_path_buf() })?;
            // An already-addRef'ed pointer the caller owns: `from_raw`'s contract.
            unsafe { ComPtr::from_raw(get_factory()) }.ok_or_else(|| Vst3Error::NoFactory { path: path.to_path_buf() })
        }))
        .map_err(|_| Vst3Error::Panicked { stage: "initialising its module" })??;
        Ok(Self { factory: opened, _library: library, path: path.to_path_buf() })
    }

    pub(crate) fn factory(&self) -> &ComPtr<IPluginFactory> {
        &self.factory
    }
}

impl std::fmt::Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Module").field("path", &self.path).finish()
    }
}

/// `IPluginFactory* PLUGIN_API GetPluginFactory()`
type GetPluginFactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;

// ---------------------------------------------------------------------------
// The platform loaders. None of them ever unloads.
// ---------------------------------------------------------------------------

#[cfg(windows)]
struct LibraryHandle {
    handle: *mut c_void,
}

#[cfg(windows)]
impl LibraryHandle {
    fn load(binary: &Path) -> Result<Self, Vst3Error> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::Diagnostics::Debug::{
            GetThreadErrorMode, SetThreadErrorMode, SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX,
        };
        use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

        let wide: Vec<u16> = binary.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // Windows otherwise shows loader failures (corrupt binary, missing
        // dependency) as a modal system dialog, which a scan worker cannot
        // answer. Suppress those for this thread only, then restore.
        //
        // SAFETY: `wide` is NUL-terminated and outlives the call; the error-mode
        // APIs act on the current thread only.
        let (handle, error) = unsafe {
            let previous = GetThreadErrorMode();
            let changed = SetThreadErrorMode(previous | SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX, std::ptr::null_mut()) != 0;
            let handle = LoadLibraryW(wide.as_ptr());
            // Restoring may replace the last error; keep the loader's first.
            let error = handle.is_null().then(std::io::Error::last_os_error);
            if changed {
                SetThreadErrorMode(previous, std::ptr::null_mut());
            }
            (handle, error)
        };
        if let Some(error) = error {
            return Err(Vst3Error::Load { path: binary.to_path_buf(), message: error.to_string() });
        }
        Ok(Self { handle: handle.cast() })
    }

    /// `InitDll()`, when exported. Optional: older modules lack it.
    unsafe fn enter(&self) -> bool {
        type InitDllFn = unsafe extern "system" fn() -> bool;
        match self.symbol::<InitDllFn>(b"InitDll\0") {
            Some(init) => init(),
            None => true,
        }
    }

    /// # Safety
    /// `F` must be the exact signature of the export and `name` NUL-terminated.
    unsafe fn symbol<F: Copy>(&self, name: &[u8]) -> Option<F> {
        use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
        debug_assert_eq!(name.last(), Some(&0));
        debug_assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*const c_void>());
        let address = GetProcAddress(self.handle.cast(), name.as_ptr())?;
        Some(std::mem::transmute_copy::<_, F>(&address))
    }
}

#[cfg(target_os = "linux")]
struct LibraryHandle {
    handle: *mut c_void,
}

#[cfg(target_os = "linux")]
impl LibraryHandle {
    fn load(binary: &Path) -> Result<Self, Vst3Error> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(binary.as_os_str().as_bytes())
            .map_err(|_| Vst3Error::Load { path: binary.to_path_buf(), message: "the path contains a NUL byte".to_owned() })?;
        // SAFETY: `path` is NUL-terminated and outlives the call. Loading runs
        // the module's static initialisers -- third-party code, unavoidably.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if handle.is_null() {
            // SAFETY: `dlerror` returns a thread-local C string or null.
            let message = unsafe {
                let raw = libc::dlerror();
                if raw.is_null() {
                    "dlopen failed".to_owned()
                } else {
                    std::ffi::CStr::from_ptr(raw).to_string_lossy().into_owned()
                }
            };
            return Err(Vst3Error::Load { path: binary.to_path_buf(), message });
        }
        Ok(Self { handle })
    }

    /// `ModuleEntry(handle)`, when exported.
    unsafe fn enter(&self) -> bool {
        type ModuleEntryFn = unsafe extern "system" fn(*mut c_void) -> bool;
        match self.symbol::<ModuleEntryFn>(b"ModuleEntry\0") {
            Some(entry) => entry(self.handle),
            None => true,
        }
    }

    /// # Safety
    /// `F` must be the exact signature of the export and `name` NUL-terminated.
    unsafe fn symbol<F: Copy>(&self, name: &[u8]) -> Option<F> {
        debug_assert_eq!(name.last(), Some(&0));
        debug_assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*const c_void>());
        let address = libc::dlsym(self.handle, name.as_ptr().cast());
        if address.is_null() {
            None
        } else {
            Some(std::mem::transmute_copy::<*mut c_void, F>(&address))
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
struct LibraryHandle;

#[cfg(not(any(windows, target_os = "linux")))]
impl LibraryHandle {
    fn load(_binary: &Path) -> Result<Self, Vst3Error> {
        Err(Vst3Error::UnsupportedPlatform)
    }

    unsafe fn enter(&self) -> bool {
        false
    }

    unsafe fn symbol<F: Copy>(&self, _name: &[u8]) -> Option<F> {
        None
    }
}

// ---------------------------------------------------------------------------
// COM apartment
// ---------------------------------------------------------------------------

/// Put the calling thread in an apartment-threaded (STA) COM apartment, once,
/// for the rest of its life.
///
/// Many VST3 plugins touch COM during load (licensing wrappers, GUI toolkits)
/// and expect the loading thread to be in an apartment; every real host does
/// this. The UI thread of a Tauri app already is (WebView2 needs it), in which
/// case this is a no-op. Never undone: tearing an apartment down under live
/// plugin objects is worse than keeping it.
pub(crate) fn enter_com_apartment() {
    #[cfg(windows)]
    {
        use std::cell::Cell;
        thread_local!(static ENTERED: Cell<bool> = const { Cell::new(false) });
        ENTERED.with(|entered| {
            if !entered.replace(true) {
                use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
                // SAFETY: no memory is passed. S_OK / S_FALSE mean "in an STA";
                // RPC_E_CHANGED_MODE means the thread chose another model, which
                // is not ours to change. Deliberately never balanced.
                let result = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
                if result != 0 && result != 1 {
                    log::debug!("this thread is already in a different COM apartment ({result:#x})");
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

/// A NUL-terminated `char8` array, lossily: a vendor string with invalid UTF-8
/// degrades to mojibake rather than hiding the plugin.
pub(crate) fn c_string_to_string(raw: &[char8]) -> String {
    let bytes: Vec<u8> = raw.iter().take_while(|byte| **byte != 0).map(|byte| *byte as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

/// A NUL-terminated UTF-16 array (VST3's `String128`).
pub(crate) fn utf16_to_string(raw: &[u16]) -> String {
    let units: Vec<u16> = raw.iter().copied().take_while(|unit| *unit != 0).collect();
    String::from_utf16_lossy(&units).trim().to_owned()
}

/// Write `text` into a `String128`-shaped buffer, NUL-terminated and truncated.
pub(crate) fn write_utf16(text: &str, out: &mut [u16]) {
    if out.is_empty() {
        return;
    }
    let last = out.len() - 1;
    let mut written = 0;
    for (slot, unit) in out[..last].iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
        written += 1;
    }
    out[written] = 0;
}

#[cfg(test)]
pub(crate) fn scratch_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wave-vst3-{tag}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("scratch dir");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_id_round_trips_through_hex_and_through_a_tuid() {
        let id = ClassId([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10]);
        let hex = id.to_hex();
        assert_eq!(hex, "0123456789ABCDEFFEDCBA9876543210");
        assert_eq!(ClassId::from_hex(&hex), Some(id));
        assert_eq!(ClassId::from_hex(&hex.to_lowercase()), Some(id));
        assert_eq!(ClassId::from_tuid(&id.to_tuid()), id);
        assert_eq!(id.to_string(), hex);
        let zero = ClassId([0; 16]);
        assert_eq!(zero.to_hex(), "0".repeat(32), "a blank id still renders all 32 digits");
    }

    #[test]
    fn a_class_id_that_is_not_32_hex_digits_is_rejected_rather_than_padded() {
        for bad in ["", "0123", "zzzz567890123456789012345678901f", &"0".repeat(33)] {
            assert!(ClassId::from_hex(bad).is_none(), "{bad} should be rejected");
        }
        assert_eq!(ClassId::from_hex("01234567-89AB-CDEF-FEDC-BA9876543210"), ClassId::from_hex("0123456789ABCDEFFEDCBA9876543210"));
    }

    #[test]
    fn a_class_id_serialises_as_its_hex_string() {
        let id = ClassId([0xAB; 16]);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", "AB".repeat(16)));
        assert_eq!(serde_json::from_str::<ClassId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<ClassId>("\"nope\"").is_err());
    }

    #[test]
    fn a_flat_file_resolves_to_itself_and_a_missing_path_is_left_to_the_loader() {
        let dir = scratch_dir("flat");
        let flat = dir.join("Thing.vst3");
        std::fs::write(&flat, b"not really a library").unwrap();
        assert_eq!(resolve_binary(&flat).unwrap(), flat);
        let missing = dir.join("Missing.vst3");
        assert_eq!(resolve_binary(&missing).unwrap(), missing);
    }

    #[test]
    fn a_bundle_resolves_to_the_binary_named_after_it_or_the_only_one_inside() {
        let dir = scratch_dir("bundle");
        let bundle = dir.join("Serum.vst3");
        let arch = bundle.join(CONTENTS_DIR).join(ARCH_DIR);
        std::fs::create_dir_all(&arch).unwrap();
        let mut binary = arch.join("Serum");
        binary.set_extension(BINARY_EXTENSION);
        std::fs::write(&binary, b"x").unwrap();
        assert_eq!(resolve_binary(&bundle).unwrap(), binary);

        let renamed = dir.join("Suite.vst3");
        let arch = renamed.join(CONTENTS_DIR).join(ARCH_DIR);
        std::fs::create_dir_all(&arch).unwrap();
        let mut core = arch.join("SuiteCore");
        core.set_extension(BINARY_EXTENSION);
        std::fs::write(&core, b"x").unwrap();
        assert_eq!(resolve_binary(&renamed).unwrap(), core);
    }

    #[test]
    fn a_bundle_with_no_binary_for_this_platform_says_where_it_looked() {
        let dir = scratch_dir("wrong-arch");
        let bundle = dir.join("Old.vst3");
        std::fs::create_dir_all(bundle.join(CONTENTS_DIR).join("ppc-mac")).unwrap();
        let err = resolve_binary(&bundle).unwrap_err();
        assert!(matches!(err, Vst3Error::NoBinary { .. }), "got {err:?}");
        assert!(err.to_string().contains(ARCH_DIR), "{err}");
    }

    #[test]
    fn the_extension_test_is_case_insensitive_and_extension_only() {
        assert!(has_vst3_extension(Path::new("a.vst3")));
        assert!(has_vst3_extension(Path::new("B.VST3")));
        assert!(!has_vst3_extension(Path::new("a.dll")));
        assert!(!has_vst3_extension(Path::new("vst3")));
    }

    #[test]
    fn opening_something_that_is_not_a_library_is_a_typed_error_not_a_panic() {
        let dir = scratch_dir("garbage");
        let bogus = dir.join("garbage.vst3");
        std::fs::write(&bogus, b"this is not a shared library").unwrap();
        let err = Module::open(&bogus).expect_err("a text file is not a VST3 module");
        assert!(matches!(err, Vst3Error::Load { .. } | Vst3Error::UnsupportedPlatform), "got {err:?}");
        assert!(Module::shared(&bogus).is_err(), "failures are not cached as successes");
    }

    #[test]
    fn strings_stop_at_the_nul_survive_bad_utf8_and_truncate_safely() {
        let mut raw = [0 as char8; 16];
        for (slot, byte) in raw.iter_mut().zip(b"Antares\0junk".iter()) {
            *slot = *byte as char8;
        }
        assert_eq!(c_string_to_string(&raw), "Antares");
        let broken = [0x41_u8 as char8, 0xFF_u8 as char8, 0];
        assert!(c_string_to_string(&broken).starts_with('A'));

        let mut buffer = [0_u16; 128];
        write_utf16("WAVE", &mut buffer);
        assert_eq!(utf16_to_string(&buffer), "WAVE");
        let mut small = [0xffff_u16; 4];
        write_utf16("WAVE Studio", &mut small);
        assert_eq!(small[3], 0, "truncation must still leave a NUL");
        assert_eq!(utf16_to_string(&small), "WAV");
        write_utf16("anything", &mut []);
    }
}
