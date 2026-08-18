//! The plugin contract. Everything in MYDAW above the microkernel is a plugin:
//! transport, mixer, CLAP/VST3 hosts, audio I/O, even UI panels.

use crate::ctx::KernelCtx;

/// Static identity of a plugin.
///
/// `&'static str` throughout: plugins are statically linked crates registered by
/// hand in `app/main.rs`, so their identity is known at compile time and needs no
/// allocation. Dynamically discovered *audio* plugins (CLAP/VST3) are not kernel
/// plugins -- they are data owned by the host plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginManifest {
    /// Unique, stable id, e.g. `"mydaw.audio-io"`. Used for dependency edges.
    pub id: &'static str,
    /// Human-readable name for the status panel.
    pub name: &'static str,
    /// Ids of plugins that must be registered and started first.
    pub deps: &'static [&'static str],
}

impl PluginManifest {
    /// Manifest with no dependencies.
    pub const fn new(id: &'static str, name: &'static str) -> Self {
        Self {
            id,
            name,
            deps: &[],
        }
    }

    /// Manifest with dependencies.
    pub const fn with_deps(
        id: &'static str,
        name: &'static str,
        deps: &'static [&'static str],
    ) -> Self {
        Self { id, name, deps }
    }
}

/// A kernel plugin.
///
/// Lifecycle, all on the main (non-RT) thread:
/// 1. [`register`](Plugin::register) -- publish services, subscribe to events,
///    contribute RT node factories and UI panels. Called in dependency order, so
///    services from dependencies are already available.
/// 2. [`start`](Plugin::start) -- resolve dependencies' services and begin work
///    (open devices, spawn threads). Also dependency order.
/// 3. [`stop`](Plugin::stop) -- release resources. Called in *reverse* order, so a
///    plugin's dependencies outlive it.
///
/// A panic in an internal plugin is a bug: per the design spec it crashes loudly
/// in dev builds rather than being swallowed.
pub trait Plugin {
    fn manifest(&self) -> PluginManifest;

    /// Contribute to the kernel. Mutable ctx: this is the only phase in which
    /// contributions (node factories, panels) may be added.
    fn register(&mut self, ctx: &mut KernelCtx);

    /// Begin doing work. Ctx is shared: services and bus only.
    fn start(&mut self, ctx: &KernelCtx);

    /// Release resources. Must tolerate being called without a prior `start`.
    fn stop(&mut self);
}

/// A UI panel contributed by a plugin.
///
/// The kernel never renders anything; it only collects manifests and ships them to
/// the webview, which looks `component` up in the frontend's panel registry. That
/// keeps the Rust side ignorant of SolidJS and the TS side ignorant of the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelManifest {
    /// Unique panel id, e.g. `"mydaw.panel.kernel-status"`.
    pub id: String,
    /// Tab/window title shown to the user.
    pub title: String,
    /// Key of the frontend component that renders this panel.
    pub component: String,
}

impl PanelManifest {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        component: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            component: component.into(),
        }
    }
}

/// Lifecycle state of a registered plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    /// Known to the kernel; not started (yet), or the kernel has not booted.
    Registered,
    /// `start()` returned successfully.
    Started,
    /// `stop()` has been called.
    Stopped,
}

/// Snapshot of one plugin's status, for the status panel / IPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStatus {
    pub id: String,
    pub name: String,
    pub state: PluginState,
}
