//! Opening a VST3 module: the shared library, its entry points, its factory.
//!
//! A "module" is what the VST3 specification calls the loadable unit. On Windows
//! it takes two shapes, and both are installed on a typical machine:
//!
//! ```text
//!  flat:    C:\Program Files\Common Files\VST3\Auto-Tune Pro.vst3      <- a DLL
//!  bundle:  C:\Program Files\Common Files\VST3\Serum.vst3\             <- a directory
//!                Contents\x86_64-win\Serum.vst3                        <- the DLL
//! ```
//!
//! [`resolve_binary`] is the rule for turning either into the path the OS loader
//! is given; it is a pure function of the path and is unit-tested without a
//! plugin in sight.
//!
//! # The entry-point protocol
//!
//! ```text
//!  LoadLibraryW(binary)
//!  InitDll()             optional; a module without it is still valid
//!  GetPluginFactory()    required; this *is* what makes a DLL a VST3 module
//!  ... use the factory ...
//!  release the factory
//!  ExitDll()             optional, and paired with InitDll
//!  FreeLibrary
//! ```
//!
//! [`Vst3Module`] performs the whole of it, in that order, and the order is why
//! it exists as a type: the factory is a COM object living in the module's
//! address space, so releasing it *after* `FreeLibrary` is a use-after-free, and
//! Rust's field-drop order (declaration order) is what guarantees it cannot
//! happen. Every type in this crate that holds both a COM pointer and a module
//! therefore declares the pointer first and the module last.
//!
//! # Crash isolation
//! Loading a module runs third-party code: the DLL's entry point, `InitDll`, and
//! whatever static initialisers the vendor linked in. That code can pop a
//! licensing dialog, take seconds, or crash. [`Vst3Module::open`] wraps its FFI
//! in [`catch_unwind`], which covers a panic and nothing else -- see
//! [`crate::scan`] for the timeout that covers "took forever", and the crate docs
//! for why only an out-of-process scanner covers the rest.

use std::ffi::{c_void, OsStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use vst3::ComPtr;
use vst3::Steinberg::{char8, IPluginFactory, TUID};

/// Sub-directory of a bundle holding the platform binaries.
pub const CONTENTS_DIR: &str = "Contents";

/// File extension of a VST3 module, bundle or flat DLL alike.
pub const VST3_EXTENSION: &str = "vst3";

/// The architecture directories inside a bundle, most preferred first.
///
/// Only the host's own architecture is ever loadable -- a 64-bit process cannot
/// load a 32-bit DLL -- so this list is one entry long per build, and exists as a
/// list only so that the rule is written in one place.
#[cfg(all(windows, target_arch = "x86_64"))]
pub const ARCH_DIRS: &[&str] = &["x86_64-win"];
#[cfg(all(windows, target_arch = "aarch64"))]
pub const ARCH_DIRS: &[&str] = &["arm64-win"];
#[cfg(all(windows, target_arch = "x86"))]
pub const ARCH_DIRS: &[&str] = &["x86-win"];
#[cfg(target_os = "macos")]
pub const ARCH_DIRS: &[&str] = &["MacOS"];
#[cfg(all(unix, not(target_os = "macos"), target_arch = "x86_64"))]
pub const ARCH_DIRS: &[&str] = &["x86_64-linux"];
#[cfg(all(unix, not(target_os = "macos"), target_arch = "aarch64"))]
pub const ARCH_DIRS: &[&str] = &["aarch64-linux"];

/// The 16-byte class id that names one plugin inside a module.
///
/// VST3's own `TUID` is `[i8; 16]`, whose byte order differs between Windows and
/// everywhere else; this type stores the bytes exactly as the factory reported
/// them and hands them back unchanged, so a round trip through a project file
/// cannot silently re-order them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClassId(pub [u8; 16]);

impl ClassId {
    /// The id a factory reported.
    pub fn from_tuid(tuid: &TUID) -> Self {
        let mut bytes = [0_u8; 16];
        for (out, raw) in bytes.iter_mut().zip(tuid.iter()) {
            *out = *raw as u8;
        }
        Self(bytes)
    }

    /// The id in the shape `IPluginFactory::createInstance` wants.
    pub fn to_tuid(self) -> TUID {
        let mut tuid: TUID = [0; 16];
        for (out, raw) in tuid.iter_mut().zip(self.0.iter()) {
            *out = *raw as char8;
        }
        tuid
    }

    /// Uppercase hex, 32 characters. This is what crosses the IPC boundary and
    /// what a project file records.
    pub fn to_hex(self) -> String {
        let mut text = String::with_capacity(32);
        for byte in self.0 {
            text.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            text.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
        text.to_uppercase()
    }

    /// Parse what [`to_hex`](Self::to_hex) wrote.
    ///
    /// Case-insensitive, and tolerant of the dashed form a user might paste from
    /// a vendor's documentation.
    ///
    /// # Errors
    /// `None` for anything that is not 32 hex digits.
    pub fn from_hex(text: &str) -> Option<Self> {
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

/// As the 32-digit hex string, not as sixteen numbers.
///
/// A class id crosses two boundaries in this shape already -- the IPC wire and a
/// project file -- and the scan cache is read by humans when a plugin goes
/// missing. A byte array would be all three of unreadable, larger, and a second
/// spelling of the same thing.
impl serde::Serialize for ClassId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for ClassId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text).ok_or_else(|| {
            serde::de::Error::custom(format!("{text:?} is not a 32-digit VST3 class id"))
        })
    }
}

/// Why a module could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum Vst3ModuleError {
    /// A bundle directory with no binary for this architecture inside it.
    /// Normal on a machine with a plugin installed for a different CPU.
    #[error(
        "VST3 bundle {path} contains no binary for this architecture (looked for {looked_in})"
    )]
    NoBinary { path: PathBuf, looked_in: String },

    /// The OS loader refused: not a DLL, wrong architecture, a missing
    /// dependency the vendor did not ship.
    #[error("could not load VST3 module {path}: {message}")]
    Load { path: PathBuf, message: String },

    /// The library loaded but exports no `GetPluginFactory` -- so it is a DLL
    /// that happens to be called `.vst3`, not a VST3 module.
    #[error("VST3 module {path} exports no GetPluginFactory")]
    NoEntryPoint { path: PathBuf },

    /// `GetPluginFactory()` returned null, which the specification does not
    /// allow but a broken or unlicensed module can still do.
    #[error("VST3 module {path} returned no plugin factory")]
    NoFactory { path: PathBuf },

    /// `InitDll()` returned false: the module declined to initialise. This is
    /// the polite failure mode of a plugin whose licence check did not pass.
    #[error("VST3 module {path} refused to initialise (InitDll returned false)")]
    InitRefused { path: PathBuf },

    /// Module code panicked through the FFI boundary. Caught, reported, skipped.
    #[error("VST3 module {path} panicked while {stage}")]
    Panicked { path: PathBuf, stage: &'static str },

    /// This platform has no VST3 loader in this build.
    #[error("VST3 modules cannot be loaded on this platform")]
    Unsupported,
}

/// Turn a `.vst3` path into the path the OS loader is given.
///
/// A flat `.vst3` file *is* the DLL. A `.vst3` directory is a bundle, and the
/// binary lives at `Contents/<arch>/<same file name>`; the specification also
/// allows a differently named binary, so a directory whose expected name is
/// missing falls back to the only `.vst3` file in the architecture directory.
///
/// # Errors
/// [`Vst3ModuleError::NoBinary`] for a bundle with nothing loadable inside.
pub fn resolve_binary(path: &Path) -> Result<PathBuf, Vst3ModuleError> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if !path.is_dir() {
        // Neither: let the loader produce the error, which names the OS's own
        // reason (missing file, permission) rather than a guess of ours.
        return Ok(path.to_path_buf());
    }

    let file_name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("plugin.vst3"));
    let contents = path.join(CONTENTS_DIR);
    for arch in ARCH_DIRS {
        let arch_dir = contents.join(arch);
        let expected = arch_dir.join(file_name);
        if expected.is_file() {
            return Ok(expected);
        }
        // A bundle whose binary is not named after the bundle. Rare, legal, and
        // cheap to support: take the only `.vst3` file in the directory.
        if let Ok(entries) = std::fs::read_dir(&arch_dir) {
            let mut candidates: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|candidate| candidate.is_file() && has_vst3_extension(candidate))
                .collect();
            candidates.sort();
            if let Some(only) = candidates.into_iter().next() {
                return Ok(only);
            }
        }
    }

    Err(Vst3ModuleError::NoBinary {
        path: path.to_path_buf(),
        looked_in: ARCH_DIRS
            .iter()
            .map(|arch| format!("{CONTENTS_DIR}/{arch}"))
            .collect::<Vec<_>>()
            .join(", "),
    })
}

/// Whether a path ends in `.vst3`, in any case.
pub fn has_vst3_extension(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(VST3_EXTENSION))
}

/// Holds a COM apartment open for as long as it exists.
///
/// # Why a host must do this before loading anything
///
/// A great many VST3 plugins touch COM during module load -- licensing wrappers
/// and GUI toolkits especially -- and expect the calling thread to already be in
/// an apartment. On a thread that never called `CoInitializeEx`, that code
/// variously fails, or blocks. Every real host initialises an
/// **apartment-threaded** (STA) apartment on the thread it loads plugins from,
/// and so does this one.
///
/// `S_FALSE` (already initialised on this thread, compatibly) is a success and is
/// still balanced by `CoUninitialize`, which is why the guard records whether it
/// has anything to undo rather than assuming.
#[cfg(windows)]
pub struct ComApartment {
    initialised: bool,
}

#[cfg(windows)]
impl ComApartment {
    /// Enter an STA on this thread.
    ///
    /// Never fails in a way a caller can act on: a thread already in a
    /// *different* apartment model keeps the one it has, which is the right
    /// answer -- the host does not own that decision.
    pub fn enter() -> Self {
        use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

        // SAFETY: `CoInitializeEx` takes no memory from us. Its contract is that
        // a successful call is balanced by exactly one `CoUninitialize` on the
        // same thread, which `Drop` does.
        let result = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        // S_OK (0) and S_FALSE (1) both mean "you are in an apartment and you owe
        // a CoUninitialize". RPC_E_CHANGED_MODE means somebody else already chose
        // a different model; we owe nothing and must not undo theirs.
        let initialised = result == 0 || result == 1;
        if !initialised {
            log::debug!("this thread is already in a different COM apartment ({result:#x})");
        }
        Self { initialised }
    }
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Com::CoUninitialize;

        if self.initialised {
            // SAFETY: balances exactly one successful `CoInitializeEx` on this
            // same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// No-op on platforms with no COM.
#[cfg(not(windows))]
pub struct ComApartment;

#[cfg(not(windows))]
impl ComApartment {
    pub fn enter() -> Self {
        Self
    }
}

/// A loaded VST3 module and its plugin factory.
///
/// **Not `Send`.** The factory is a COM object created by whatever thread called
/// `GetPluginFactory`, and everything this crate does with a factory (enumerating
/// classes, creating instances) is main-thread work in the same sense CLAP means
/// it. The one deliberate exception is [`crate::scan`], which does the whole
/// load/enumerate/unload cycle inside one worker thread and only sends the
/// *results* back.
pub struct Vst3Module {
    // Declaration order is drop order, and drop order is the protocol: the
    // factory must be released, then `ExitDll` called, then the library freed.
    factory: Option<ComPtr<IPluginFactory>>,
    library: LibraryHandle,
    path: PathBuf,
    binary: PathBuf,
}

impl Vst3Module {
    /// Load the module at `path` and take its factory.
    ///
    /// `path` is the `.vst3` bundle or flat DLL; the binary inside is resolved by
    /// [`resolve_binary`].
    ///
    /// # Errors
    /// Every [`Vst3ModuleError`]. None of them is fatal to the caller: a module
    /// that will not open is reported and skipped.
    pub fn open(path: &Path) -> Result<Self, Vst3ModuleError> {
        let binary = resolve_binary(path)?;
        let library = LibraryHandle::load(&binary)?;

        // SAFETY (the whole block): `library` is a live module handle, and each
        // symbol is called with the signature the VST3 specification declares
        // for it. A module that lies about those signatures corrupts the stack,
        // which no host can defend against; that is what the out-of-process scan
        // in the crate docs is for.
        let opened = catch_unwind(AssertUnwindSafe(|| -> Result<_, Vst3ModuleError> {
            // `InitDll` is optional: plenty of modules built with older SDKs do
            // not export it, and refusing those would refuse working plugins.
            if let Some(init) = unsafe { library.symbol::<InitDllFn>(b"InitDll\0") } {
                if !unsafe { init() } {
                    return Err(Vst3ModuleError::InitRefused {
                        path: path.to_path_buf(),
                    });
                }
            }

            let get_factory =
                unsafe { library.symbol::<GetPluginFactoryFn>(b"GetPluginFactory\0") }.ok_or_else(
                    || Vst3ModuleError::NoEntryPoint {
                        path: path.to_path_buf(),
                    },
                )?;

            // `GetPluginFactory` returns an already-`addRef`ed pointer that the
            // caller owns, which is exactly `ComPtr::from_raw`'s contract.
            let raw = unsafe { get_factory() };
            let factory =
                unsafe { ComPtr::from_raw(raw) }.ok_or_else(|| Vst3ModuleError::NoFactory {
                    path: path.to_path_buf(),
                })?;
            Ok(factory)
        }))
        .map_err(|_| Vst3ModuleError::Panicked {
            path: path.to_path_buf(),
            stage: "initialising",
        })?;

        match opened {
            Ok(factory) => Ok(Self {
                factory: Some(factory),
                library,
                path: path.to_path_buf(),
                binary,
            }),
            Err(err) => {
                // `library` is dropped here, which frees it -- correct, because
                // no COM object from it survived.
                Err(err)
            }
        }
    }

    /// The module's plugin factory.
    pub fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory
            .as_ref()
            .expect("the factory is only taken in Drop")
    }

    /// The `.vst3` path this module was opened from (bundle or flat DLL).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The DLL actually handed to the OS loader.
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Never unload this module.
    ///
    /// The escape hatch for the one case a host cannot otherwise survive: a COM
    /// object from this module has outlived the host's ability to reclaim it (an
    /// RT node that never came home from the collector, a plugin that panicked
    /// on the way out). Unloading now would turn that object's next `release`
    /// into a call through a freed page.
    ///
    /// A leaked DLL is a bounded, measurable cost. A use-after-free in an audio
    /// callback is not, so this is the right trade every time it comes up --
    /// which, on a healthy shutdown, is never.
    pub fn leak(&mut self) {
        self.library.leak();
    }
}

impl std::fmt::Debug for Vst3Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Module")
            .field("path", &self.path)
            .field("binary", &self.binary)
            .finish()
    }
}

/// Environment override: set to `1` to actually unload modules on drop.
///
/// See [`should_unload_modules`]. For experiments and bug reports, not for
/// ordinary use.
pub const UNLOAD_MODULES_ENV: &str = "MYDAW_VST3_UNLOAD_MODULES";

/// Whether a module should be unloaded when the last handle to it is dropped.
///
/// **`false` by default, and this is not timidity -- it is a measured result.**
///
/// On the development machine, an in-process scan that unloaded each module
/// after reading it crashed with an access violation at the *eleventh* module:
/// `AVOX ARTICULATOR` and `AVOX ASPIRE` are separate `.vst3` files that share an
/// Antares runtime, and freeing the first tore down state the second then used.
/// Loading either one alone is perfectly fine, which is exactly what makes this
/// class of bug so easy to ship.
///
/// This is not an Antares quirk. `ExitDll` + `FreeLibrary` runs a vendor's global
/// teardown, and a great deal of shipping plugin code has never been run through
/// it -- because the hosts that matter do not do it either. The VST3 SDK's own
/// module wrapper caches modules per path for the life of the process, and every
/// major DAW scans out of process precisely so that unloading is somebody else's
/// problem.
///
/// So MYDAW keeps modules loaded. The cost is address space -- file-backed
/// mappings that the OS can page out, released when the process exits -- and it
/// is bounded by how many plugins are installed. The alternative cost is a
/// crashed DAW and a lost session.
pub fn should_unload_modules() -> bool {
    std::env::var_os(UNLOAD_MODULES_ENV).is_some_and(|value| value == "1")
}

impl Drop for Vst3Module {
    fn drop(&mut self) {
        // The factory is refcounted and released either way: that is ordinary
        // COM, and it is what tells the module nobody is using it.
        drop(self.factory.take());

        if !should_unload_modules() {
            // See `should_unload_modules`. Leaking here is the default, and the
            // `ExitDll` below is skipped with it -- a module told to tear down
            // its globals while still mapped would refuse the *next*
            // `GetPluginFactory` in this process.
            self.library.leak();
            return;
        }

        // SAFETY: `library` is still loaded; `ExitDll` takes no arguments and is
        // the documented counterpart of the `InitDll` `open` may have called.
        if let Some(exit) = unsafe { self.library.symbol::<ExitDllFn>(b"ExitDll\0") } {
            if catch_unwind(AssertUnwindSafe(|| unsafe { exit() })).is_err() {
                log::error!(
                    "VST3 module {} panicked in ExitDll; it will not be unloaded",
                    self.path.display()
                );
                self.library.leak();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The platform loader
// ---------------------------------------------------------------------------

/// `bool PLUGIN_API InitDll()`
type InitDllFn = unsafe extern "system" fn() -> bool;
/// `bool PLUGIN_API ExitDll()`
type ExitDllFn = unsafe extern "system" fn() -> bool;
/// `IPluginFactory* PLUGIN_API GetPluginFactory()`
type GetPluginFactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;

#[cfg(windows)]
struct LibraryHandle {
    handle: *mut c_void,
}

#[cfg(windows)]
impl LibraryHandle {
    fn load(binary: &Path) -> Result<Self, Vst3ModuleError> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::Diagnostics::Debug::{
            GetThreadErrorMode, SetThreadErrorMode, SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX,
        };
        use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

        let wide: Vec<u16> = binary
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Windows otherwise presents loader failures (a corrupt binary, wrong
        // architecture, or missing dependency) as a modal system dialog. A scan
        // worker cannot answer that dialog, so it would sit behind it until the
        // parent's deadline expired. Suppress only those loader dialogs for this
        // thread and restore whatever mode its caller had immediately afterward.
        //
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call.
        // Loading runs the module's entry point and its static initialisers --
        // third-party code, unavoidably. The error-mode APIs operate only on the
        // current thread, and `previous_mode` is a valid out pointer.
        let (handle, load_error) = unsafe {
            let previous_mode = GetThreadErrorMode();
            let quiet_mode = previous_mode | SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX;
            let mode_changed = SetThreadErrorMode(quiet_mode, std::ptr::null_mut()) != 0;

            let handle = LoadLibraryW(wide.as_ptr());
            // Restoring the mode is another Win32 call and may replace the last
            // error, so preserve the loader's explanation first.
            let load_error = handle.is_null().then(std::io::Error::last_os_error);

            if mode_changed {
                SetThreadErrorMode(previous_mode, std::ptr::null_mut());
            }
            (handle, load_error)
        };
        if let Some(error) = load_error {
            return Err(Vst3ModuleError::Load {
                path: binary.to_path_buf(),
                message: error.to_string(),
            });
        }
        Ok(Self {
            handle: handle.cast(),
        })
    }

    /// Look up an exported symbol.
    ///
    /// # Safety
    /// `F` must be the exact signature the module exports the symbol with, and
    /// `name` must be NUL-terminated. Getting either wrong is undefined
    /// behaviour; the three call sites use the signatures from the VST3
    /// specification.
    unsafe fn symbol<F: Copy>(&self, name: &[u8]) -> Option<F> {
        use windows_sys::Win32::System::LibraryLoader::GetProcAddress;

        debug_assert_eq!(name.last(), Some(&0), "symbol names are NUL-terminated");
        debug_assert_eq!(
            std::mem::size_of::<F>(),
            std::mem::size_of::<*const c_void>(),
            "a symbol is transmuted to a function pointer, which must be thin"
        );
        let address = GetProcAddress(self.handle.cast(), name.as_ptr())?;
        Some(std::mem::transmute_copy::<_, F>(&address))
    }

    /// Give up on unloading. Used when the module misbehaved on the way out:
    /// leaking a DLL is a bounded cost, and freeing one whose teardown just
    /// panicked is a crash.
    fn leak(&mut self) {
        self.handle = std::ptr::null_mut();
    }
}

#[cfg(windows)]
impl Drop for LibraryHandle {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::FreeLibrary;

        if self.handle.is_null() {
            return;
        }
        // SAFETY: the handle came from `LoadLibraryW` and every COM object taken
        // from this module has been released by now -- the field order of
        // `Vst3Module`, and of every type that owns one, is what guarantees it.
        unsafe {
            FreeLibrary(self.handle.cast());
        }
    }
}

#[cfg(not(windows))]
struct LibraryHandle;

#[cfg(not(windows))]
impl LibraryHandle {
    fn load(_binary: &Path) -> Result<Self, Vst3ModuleError> {
        Err(Vst3ModuleError::Unsupported)
    }

    /// # Safety
    /// Never called: [`load`](Self::load) always fails on this platform.
    unsafe fn symbol<F: Copy>(&self, _name: &[u8]) -> Option<F> {
        None
    }

    fn leak(&mut self) {}
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

/// Read a NUL-terminated `char8` array as the factory filled it.
///
/// Lossy on purpose: a vendor string with invalid UTF-8 should degrade to
/// mojibake in the browser, not make the plugin invisible.
pub fn c_string_to_string(raw: &[char8]) -> String {
    let bytes: Vec<u8> = raw
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

/// Read a NUL-terminated UTF-16 array (VST3's `String128` and friends).
pub fn utf16_to_string(raw: &[u16]) -> String {
    let units: Vec<u16> = raw.iter().copied().take_while(|unit| *unit != 0).collect();
    String::from_utf16_lossy(&units).trim().to_owned()
}

/// Write `text` into a `String128`-shaped buffer, NUL-terminated and truncated.
///
/// Used for the host's own name, which is the only string this host ever hands
/// *to* a plugin.
pub fn write_utf16(text: &str, out: &mut [u16]) {
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
mod tests {
    use super::*;

    #[test]
    fn a_class_id_round_trips_through_hex_and_through_a_tuid() {
        let id = ClassId([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ]);
        let hex = id.to_hex();
        assert_eq!(hex, "0123456789ABCDEFFEDCBA9876543210");
        assert_eq!(hex.len(), 32);
        assert_eq!(ClassId::from_hex(&hex), Some(id));
        assert_eq!(
            ClassId::from_hex("0123456789abcdeffedcba9876543210"),
            Some(id),
            "hex parsing is case-insensitive"
        );
        assert_eq!(ClassId::from_tuid(&id.to_tuid()), id);
        assert_eq!(id.to_string(), hex);
    }

    #[test]
    fn a_zero_class_id_still_renders_all_32_digits() {
        // A factory that leaves the cid blank must not produce an empty string
        // that a project file would then read back as "no plugin at all".
        let zero = ClassId([0; 16]);
        assert_eq!(zero.to_hex(), "00000000000000000000000000000000");
        assert_eq!(ClassId::from_hex(&zero.to_hex()), Some(zero));
    }

    #[test]
    fn a_class_id_that_is_not_32_hex_digits_is_rejected_rather_than_padded() {
        // Reachable from a hand-edited project file.
        for bad in [
            "",
            "0123",
            "zzzz567890123456789012345678901f",
            &"0".repeat(33),
        ] {
            assert!(ClassId::from_hex(bad).is_none(), "{bad} should be rejected");
        }
        // Dashes and spaces are what a user pastes from vendor documentation.
        assert_eq!(
            ClassId::from_hex("01234567-89AB-CDEF-FEDC-BA9876543210"),
            ClassId::from_hex("0123456789ABCDEFFEDCBA9876543210")
        );
    }

    #[test]
    fn a_flat_dll_resolves_to_itself() {
        let dir = scratch("flat");
        let flat = dir.join("Thing.vst3");
        std::fs::write(&flat, b"not really a dll").unwrap();
        assert_eq!(resolve_binary(&flat).unwrap(), flat);
    }

    #[test]
    fn a_bundle_resolves_to_the_binary_named_after_it() {
        let dir = scratch("bundle");
        let bundle = dir.join("Serum.vst3");
        let arch = bundle.join(CONTENTS_DIR).join(ARCH_DIRS[0]);
        std::fs::create_dir_all(&arch).unwrap();
        let binary = arch.join("Serum.vst3");
        std::fs::write(&binary, b"not really a dll").unwrap();

        assert_eq!(resolve_binary(&bundle).unwrap(), binary);
    }

    #[test]
    fn a_bundle_whose_binary_has_another_name_still_resolves() {
        // Legal per the specification, and the reason the lookup is not just a
        // string join.
        let dir = scratch("renamed");
        let bundle = dir.join("Suite.vst3");
        let arch = bundle.join(CONTENTS_DIR).join(ARCH_DIRS[0]);
        std::fs::create_dir_all(&arch).unwrap();
        let binary = arch.join("SuiteCore.vst3");
        std::fs::write(&binary, b"not really a dll").unwrap();

        assert_eq!(resolve_binary(&bundle).unwrap(), binary);
    }

    #[test]
    fn a_bundle_with_no_binary_for_this_architecture_says_where_it_looked() {
        let dir = scratch("wrong-arch");
        let bundle = dir.join("Old.vst3");
        std::fs::create_dir_all(bundle.join(CONTENTS_DIR).join("ppc-mac")).unwrap();

        let err = resolve_binary(&bundle).unwrap_err();
        assert!(
            matches!(err, Vst3ModuleError::NoBinary { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains(ARCH_DIRS[0]), "{err}");
    }

    #[test]
    fn a_path_that_exists_at_all_is_handed_to_the_loader_verbatim() {
        // So that "the file is not there" is reported by the OS, in the OS's own
        // words, rather than guessed at here.
        let missing = std::env::temp_dir().join("mydaw-no-such-module-3fa1.vst3");
        assert_eq!(resolve_binary(&missing).unwrap(), missing);
    }

    #[test]
    fn the_extension_test_is_case_insensitive_and_extension_only() {
        assert!(has_vst3_extension(Path::new("a.vst3")));
        assert!(has_vst3_extension(Path::new(r"C:\x\B.VST3")));
        assert!(!has_vst3_extension(Path::new("a.dll")));
        assert!(!has_vst3_extension(Path::new("vst3")));
    }

    #[test]
    fn opening_something_that_is_not_a_library_is_a_typed_error_not_a_panic() {
        let dir = scratch("garbage");
        let bogus = dir.join("garbage.vst3");
        std::fs::write(&bogus, b"this is not a shared library").unwrap();

        let err = Vst3Module::open(&bogus).expect_err("a text file is not a VST3 module");
        assert!(
            matches!(
                err,
                Vst3ModuleError::Load { .. } | Vst3ModuleError::Unsupported
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn c_strings_stop_at_the_nul_and_survive_invalid_utf8() {
        let mut raw = [0 as char8; 16];
        for (slot, byte) in raw.iter_mut().zip(b"Antares\0junk".iter()) {
            *slot = *byte as char8;
        }
        assert_eq!(c_string_to_string(&raw), "Antares");
        // A lone continuation byte: mojibake, not an empty vendor field.
        let broken = [0x41 as char8, -1_i8 as char8, 0];
        assert!(c_string_to_string(&broken).starts_with('A'));
        assert_eq!(c_string_to_string(&[0 as char8; 8]), "");
    }

    #[test]
    fn utf16_strings_round_trip_through_the_host_name_buffer() {
        let mut buffer = [0_u16; 128];
        write_utf16("MYDAW", &mut buffer);
        assert_eq!(utf16_to_string(&buffer), "MYDAW");

        // Truncation must still leave a NUL, or the plugin reads off the end.
        let mut small = [0xffff_u16; 4];
        write_utf16("MYDAW", &mut small);
        assert_eq!(small[3], 0);
        assert_eq!(utf16_to_string(&small), "MYD");

        write_utf16("anything", &mut []);
    }

    /// Self-deleting scratch directory, as `host-clap`'s tests use.
    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("mydaw-vst3-{tag}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        path
    }
}
