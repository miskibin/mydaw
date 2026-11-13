//! The one error type of this crate.
//!
//! Every variant is reachable from plugin-controlled data or from the user's
//! machine, and none of them panics: a broken plugin must produce an `Err`,
//! because a panic across an FFI boundary is undefined behaviour and a panic in
//! the DAW is a lost session. Messages are sentences a plugin browser or status
//! bar can show as-is.

use std::path::PathBuf;

/// Why a VST3 operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Vst3Error {
    /// A bundle directory with no binary for this architecture inside it.
    #[error("VST3 bundle {path} contains no binary for this platform (looked in {looked_in})")]
    NoBinary { path: PathBuf, looked_in: String },

    /// The OS loader refused: not a library, wrong architecture, a missing
    /// dependency the vendor did not ship.
    #[error("could not load VST3 module {path}: {message}")]
    Load { path: PathBuf, message: String },

    /// The library exports no `GetPluginFactory`.
    #[error("VST3 module {path} exports no GetPluginFactory")]
    NoEntryPoint { path: PathBuf },

    /// `GetPluginFactory()` returned null.
    #[error("VST3 module {path} returned no plugin factory")]
    NoFactory { path: PathBuf },

    /// `InitDll` / `ModuleEntry` returned false -- typically a licence check.
    #[error("VST3 module {path} refused to initialise")]
    InitRefused { path: PathBuf },

    /// This platform has no VST3 loader in this build (macOS bundles).
    #[error("VST3 modules cannot be loaded on this platform")]
    UnsupportedPlatform,

    /// A class id that is not 32 hex digits.
    #[error("{0:?} is not a 32-digit VST3 class id")]
    InvalidClassId(String),

    /// The module has no class with that id.
    #[error("VST3 module {path} contains no plugin class {cid}")]
    ClassNotFound { path: PathBuf, cid: String },

    /// `IPluginFactory::createInstance` refused or returned nothing.
    #[error("could not create VST3 plugin {cid}: the factory returned {result:#x}")]
    CreateInstance { cid: String, result: i32 },

    /// `IComponent::initialize` refused (the usual answer of an unlicensed plugin).
    #[error("VST3 plugin '{name}' refused to initialise (result {result:#x})")]
    Initialize { name: String, result: i32 },

    /// The component exposes no `IAudioProcessor`.
    #[error("VST3 plugin '{name}' exposes no IAudioProcessor")]
    NoAudioProcessor { name: String },

    /// The plugin cannot process 32-bit float, the engine's only format.
    #[error("VST3 plugin '{name}' cannot process 32-bit float samples")]
    NoFloat32 { name: String },

    /// The plugin's buses are not a stereo insert effect's.
    #[error("VST3 plugin '{name}' is not a stereo effect: {message}")]
    NotAStereoEffect { name: String, message: String },

    /// A sample rate or block size no plugin can be activated with.
    #[error("invalid audio configuration: {0}")]
    InvalidAudioConfig(String),

    /// `setupProcessing`, `setActive` or `setProcessing` refused.
    #[error("VST3 plugin '{name}' refused while {stage} (result {result:#x})")]
    Activate { name: String, stage: &'static str, result: i32 },

    /// `getState` / `setState` refused.
    #[error("VST3 plugin '{name}' refused to {direction} its state (result {result:#x})")]
    State { name: String, direction: &'static str, result: i32 },

    /// A state blob that is malformed or past [`MAX_STATE_BYTES`](crate::MAX_STATE_BYTES).
    #[error("invalid plugin state: {0}")]
    InvalidState(String),

    /// A parameter value outside 0..=1, or not finite.
    #[error("parameter value {0} is not a normalized value in 0..=1")]
    InvalidParamValue(f64),

    /// The parameter mailbox to the audio thread is full; the change was dropped.
    #[error("too many parameter changes are queued for the audio thread")]
    ParamQueueFull,

    /// The plugin has no edit controller, or its controller returned no view.
    /// Common and not a malfunction: plenty of effects have no editor.
    #[error("this plugin has no editor")]
    NoEditor,

    /// The editor will not embed in this platform's window type.
    #[error("this plugin's editor does not support the {api} window type")]
    UnsupportedEditorApi { api: &'static str },

    /// `IPlugView::attached` refused.
    #[error("the plugin editor refused while {stage} (result {result:#x})")]
    Editor { stage: &'static str, result: i32 },

    /// The native window could not be created or driven.
    #[error("the plugin window could not be opened: {0}")]
    Window(#[from] wave_plugin_windows::WindowError),

    /// An editor call made off the UI thread.
    #[error("plugin editors must be opened and closed on the UI thread")]
    WrongThread,

    /// Plugin (or host) code panicked inside a plugin call. Caught; the plugin
    /// is not used for that operation.
    #[error("the VST3 plugin panicked while {stage}")]
    Panicked { stage: &'static str },

    /// The main-thread executor dropped the job without running it (the event
    /// loop has exited).
    #[error("the UI thread is no longer running plugin work")]
    MainThreadUnavailable,

    /// The instance has already been torn down.
    #[error("the VST3 plugin instance has been closed")]
    Closed,
}
