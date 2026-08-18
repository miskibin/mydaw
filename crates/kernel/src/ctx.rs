//! [`KernelCtx`] -- what a plugin sees of the kernel.

use mydaw_rt_core::RtNodeFactory;

use crate::bus::EventBus;
use crate::plugin::PanelManifest;
use crate::services::ServiceRegistry;

/// Contributed RT node factory: `(type id, factory)`.
///
/// The key is a `String` copy of [`RtNodeFactory::type_id`] so lookups (and
/// project files, which store the id) do not need the factory itself.
pub type RtNodeFactoryEntry = (String, Box<dyn RtNodeFactory>);

/// Handed to every plugin during `register`/`start`.
///
/// Two kinds of member:
/// - **Runtime facilities**: [`services`](Self::services) and [`bus`](Self::bus),
///   usable for the whole process lifetime.
/// - **Contribution collectors**: [`rt_node_factories`](Self::rt_node_factories)
///   and [`ui_panels`](Self::ui_panels), filled during `register` and read by the
///   app afterwards (to build the audio graph and to tell the webview which panels
///   exist).
///
/// **Non-RT.** Nothing here may be touched from an audio callback.
#[derive(Default)]
pub struct KernelCtx {
    /// Typed service registry (dependency injection).
    pub services: ServiceRegistry,
    /// Non-RT typed event bus.
    pub bus: EventBus,
    /// RT node factories contributed by plugins, in registration order.
    pub rt_node_factories: Vec<RtNodeFactoryEntry>,
    /// UI panels contributed by plugins, in registration order.
    pub ui_panels: Vec<PanelManifest>,
}

impl KernelCtx {
    pub fn new() -> Self {
        Self::default()
    }

    /// Contribute an RT node factory. Keyed by the factory's own `type_id()`.
    ///
    /// Duplicate ids are not rejected here: the graph builder resolves by id and a
    /// later contribution shadowing an earlier one is a legitimate override path
    /// (e.g. a debug node replacing a stock one). It is logged so it is not silent.
    pub fn register_rt_node_factory(&mut self, factory: Box<dyn RtNodeFactory>) {
        let id = factory.type_id().to_owned();
        if self.rt_node_factories.iter().any(|(known, _)| known == &id) {
            log::warn!("RT node factory '{id}' registered more than once; last one wins on lookup");
        }
        self.rt_node_factories.push((id, factory));
    }

    /// Contribute a UI panel.
    pub fn contribute_panel(&mut self, manifest: PanelManifest) {
        self.ui_panels.push(manifest);
    }

    /// Look up a contributed factory by node type id. Last registration wins.
    pub fn rt_node_factory(&self, type_id: &str) -> Option<&dyn RtNodeFactory> {
        self.rt_node_factories
            .iter()
            .rev()
            .find(|(id, _)| id == type_id)
            .map(|(_, factory)| factory.as_ref())
    }
}
