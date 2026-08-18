//! `mydaw-plugin-host-vst3` -- VST3 hosting. **Stub.**
//!
//! Registered from boot day one so that the plugin list, the dependency graph and
//! the status panel show the real shape of the system, and so that the crate that
//! will hold the VST3 host already exists when spike 5 starts. It contributes
//! nothing: no services, no node factories, no panels.
//!
//! # Why a stub rather than nothing
//! The scaffold spec (`docs/superpowers/specs/2026-08-18-mydaw-scaffold-design.md`)
//! puts VST3 last in the risk order: CLAP is the primary format, VST3 is the
//! compatibility tail. Registering an empty plugin now keeps `app/main.rs` stable
//! when the implementation arrives -- the wiring line never changes, only this
//! crate's insides do.
//!
//! # Planned scope -- Phase 1, spike 5
//! Per the spec, "minimal load + process via vst3 crate":
//!
//! - Dependency: `vst3 = "0.3"` (raw COM-style bindings; there is no safe
//!   high-level host crate with a compatible licence at time of writing).
//! - Scan the standard Windows VST3 locations
//!   (`%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3`),
//!   enumerate classes through `IPluginFactory`.
//! - Instantiate one `IComponent` + `IAudioProcessor`, negotiate a stereo bus
//!   layout and 32-bit float processing, and wrap it behind
//!   [`mydaw_kernel::RtNode`] exactly like the CLAP host does. The `RtNode` seam
//!   is the whole point: the graph must not learn that two plugin formats exist.
//! - Discovery shares the crash-isolation problem described in
//!   `mydaw-plugin-host-clap`: an in-process scan cannot survive a plugin that
//!   segfaults during load. The out-of-process scanner planned there is format
//!   agnostic and will serve both hosts.
//!
//! Nothing above is implemented yet. Deliberately.

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "host-vst3";

/// RT node type id this host will claim once it can build nodes.
///
/// Declared now so the constant has one home and project files written by a later
/// build stay readable by this one (as "unknown node type" rather than garbage).
pub const NODE_TYPE_ID: &str = "vst3";

/// VST3 host. Contributes nothing until Phase 1 spike 5.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostVst3Plugin;

impl HostVst3Plugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for HostVst3Plugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, "VST3 Host")
    }

    /// No contributions yet. See the crate docs for the planned scope.
    fn register(&mut self, _ctx: &mut KernelCtx) {}

    fn start(&mut self, _ctx: &KernelCtx) {}

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;

    #[test]
    fn boots_and_contributes_nothing() {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(HostVst3Plugin::new())).unwrap();
        kernel.boot().unwrap();

        assert_eq!(kernel.boot_order(), vec![PLUGIN_ID]);
        assert!(kernel.ui_panels().is_empty());
        assert!(kernel.ctx().services.is_empty());
        assert!(kernel.ctx().rt_node_factory(NODE_TYPE_ID).is_none());
    }
}
