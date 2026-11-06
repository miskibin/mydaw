//! `wave-vst3` -- VST3 plugins as stereo insert effects for the WAVE engine.
//!
//! A hosted VST3 plugin is split in two:
//!
//! - a [`Vst3Effect`] (returned as `Box<dyn wave_engine::Processor>`), which
//!   the engine puts in a mixer insert's FX chain and the audio thread runs --
//!   real-time safe, no locks, no allocation;
//! - a [`Vst3Handle`] (`Send + Sync`, cheap clone) the app keeps per effect for
//!   everything else: parameters, state, the editor window.
//!
//! # The whole API
//!
//! ```ignore
//! // discovery (crash-proof: unknown modules are opened in child processes)
//! pub fn default_search_paths() -> Vec<PathBuf>;
//! pub fn scan(rescan: bool) -> Vec<PluginInfo>;      // effects only; cached
//! pub fn run_scan_worker_if_requested() -> bool;     // first thing in main()
//!
//! // hosting
//! pub fn set_main_thread_executor(post: impl Fn(MainThreadJob) + Send + Sync + 'static) -> bool;
//! pub fn instantiate(path: &str, cid: &str, state: Option<&[u8]>, sample_rate: f32, max_block: usize)
//!     -> Result<(Box<dyn wave_engine::Processor>, Vst3Handle), Vst3Error>;
//!
//! impl Vst3Handle {
//!     pub fn info(&self) -> &PluginInfo;
//!     pub fn params(&self) -> Vec<ParamInfo>;                       // {id, name, value, display, def}
//!     pub fn set_param(&self, id: u32, value: f64) -> Result<(), Vst3Error>;   // normalized 0..=1
//!     pub fn get_state(&self) -> Result<Vec<u8>, Vst3Error>;
//!     pub fn take_state_changed(&self) -> bool;
//!     pub fn has_editor(&self) -> bool;
//!     pub fn is_editor_open(&self) -> bool;
//!     pub fn open_editor(&self, windows: &wave_plugin_windows::Service) -> Result<(), Vst3Error>; // UI thread
//!     pub fn close_editor(&self) -> Result<(), Vst3Error>;                                        // UI thread
//! }
//! ```
//!
//! [`PluginInfo`] and [`ParamInfo`] serialise to the camelCase shapes of the
//! `vst3_scan` / `vst3_params` Tauri commands in `docs/architecture.md`.
//!
//! # How the app wires it (Tauri)
//!
//! **1. `main()`, before anything else** -- the app binary is its own scan
//! worker:
//!
//! ```ignore
//! fn main() {
//!     if wave_vst3::run_scan_worker_if_requested() { return; } // a worker exits inside
//!     tauri::Builder::default() /* ... */
//! }
//! ```
//!
//! **2. `setup` (runs on the main thread)** -- route plugin work to the UI
//! thread, and create the window service there:
//!
//! ```ignore
//! .setup(|app| {
//!     let handle = app.handle().clone();
//!     wave_vst3::set_main_thread_executor(move |job| { let _ = handle.run_on_main_thread(job); });
//!     app.manage(wave_plugin_windows::Service::new());
//!     Ok(())
//! })
//! ```
//!
//! **3. Commands.**
//! - `vst3_scan {rescan}` -> [`scan`]. Slow on a cold rescan: use an `async`
//!   command (a worker thread), never the UI thread.
//! - Building an insert chain (`putFx` with a `vst3` effect, project open) ->
//!   [`instantiate`] off the audio thread and off the UI thread (it blocks
//!   while the UI thread runs the plugin's setup); hand the processor to the
//!   engine like any built-in effect, keep the [`Vst3Handle`] by fx id. Store
//!   [`Vst3Handle::get_state`] in the project's `plugin.state` on save (and
//!   whenever [`Vst3Handle::take_state_changed`] says so).
//! - `vst3_params {fxId}` -> [`Vst3Handle::params`]; `vst3_set_param` ->
//!   [`Vst3Handle::set_param`] (never blocks). From an `async` command.
//! - `vst3_editor {fxId, open}` -> must run on the UI thread:
//!
//!   ```ignore
//!   let app2 = app.clone();
//!   app.run_on_main_thread(move || {
//!       let windows = app2.state::<wave_plugin_windows::Service>();
//!       let result = if open { handle.open_editor(&windows) } else { handle.close_editor() };
//!       // report `result` back through a channel if the command should await it
//!   })?;
//!   ```
//!   An open editor services itself (resize, DPI, the close button) from its
//!   window's timer; there is no per-frame call. [`Vst3Handle::is_editor_open`]
//!   tells the UI when the user closed it.
//!
//! **4. Shutdown (`RunEvent::Exit`, main thread)**, in this order:
//! `windows.close_all()` (editors detach before their windows die), stop the
//! audio stream and drop the engine graph (the [`Vst3Effect`]s), drop the
//! handles. The plugin instances are then torn down right there, on the UI
//! thread.
//!
//! # Threading rules in one place
//!
//! | what | thread |
//! | --- | --- |
//! | `Processor::process` | audio thread (RT-safe) |
//! | `Processor::prepare` | engine, off the audio thread; re-activates on the UI thread (blocking) if the rate or block grows |
//! | dropping a `Vst3Effect` | any thread but the audio thread |
//! | `instantiate`, `params`, `get_state` | any non-RT thread; block on a UI-thread job. Never call them while holding a lock the UI thread may wait on |
//! | `set_param`, `take_state_changed`, `is_editor_open`, `info` | any thread, non-blocking |
//! | `open_editor`, `close_editor`, `wave_plugin_windows::Service` | the UI thread only |
//! | `scan` | a worker thread |
//!
//! Without an executor (tests, a headless tool) plugin work simply runs on the
//! calling thread.
//!
//! # Crash isolation, and what it does not cover
//!
//! Scanning runs third-party code in child processes (see [`scan`]): a module
//! that crashes or hangs (a licence dialog) is block-listed, persisted, and
//! never opened again until its line is removed from the block list file. A
//! scan of an unchanged machine opens nothing. *Hosting* necessarily loads the
//! plugin into WAVE's process; a plugin that crashes while processing takes the
//! app with it, as in every DAW without per-plugin sandboxing.
//!
//! # Platforms
//!
//! Windows is the target: module loading, COM apartment, editor windows.
//! Linux loads `.vst3` bundles (`Contents/x86_64-linux/*.so`) and hosts them
//! headless (no editor windows). macOS bundles are not loadable in this build.

mod editor;
mod effect;
mod error;
mod host;
mod main_thread;
mod module;
mod scan;
mod state;

pub use effect::{instantiate, ParamInfo, Vst3Effect, Vst3Handle};
pub use error::Vst3Error;
pub use host::MAX_STATE_BYTES;
pub use main_thread::{on_main_thread, set_main_thread_executor, MainThreadJob};
pub use scan::{
    default_search_paths, run_scan_worker_if_requested, scan, PluginInfo, SCAN_WORKER_BIN,
};

/// Test seams. Not part of the API; used by this crate's integration tests.
#[doc(hidden)]
pub mod testing {
    use std::path::{Path, PathBuf};

    use wave_plugin_scan::{ModuleScan, WorkerSpawn};

    use crate::scan::ScannedClass;

    /// One scan pass over `roots` with an in-memory cache and block list,
    /// opening modules with `worker` (or in this process when `None`).
    /// Returns the effects found, the child processes started and the
    /// modules block-listed by this pass.
    pub fn scan_roots(
        roots: &[PathBuf],
        worker: Option<&Path>,
    ) -> (Vec<crate::PluginInfo>, usize, Vec<PathBuf>) {
        let spawn = worker.map_or(WorkerSpawn::InProcess, |path| {
            WorkerSpawn::Program(path.to_path_buf())
        });
        let scanner = ModuleScan::<ScannedClass>::in_memory().with_spawn(spawn);
        let modules = crate::scan::module_paths(roots);
        let pass = scanner.scan_all(&modules);
        let effects = crate::scan::scan_with(&scanner, roots, false);
        (effects, pass.children, pass.newly_blocked)
    }

    /// [`instantiate`](crate::instantiate) returning the concrete effect.
    pub fn instantiate_effect(
        path: &str,
        cid: &str,
        state: Option<&[u8]>,
        sample_rate: f32,
        max_block: usize,
    ) -> Result<(crate::Vst3Effect, crate::Vst3Handle), crate::Vst3Error> {
        crate::effect::instantiate_effect(path, cid, state, sample_rate, max_block)
    }
}
