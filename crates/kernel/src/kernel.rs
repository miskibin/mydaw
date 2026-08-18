//! [`Kernel`] -- plugin registry, dependency ordering, lifecycle.

use std::collections::{HashMap, VecDeque};

use crate::ctx::KernelCtx;
use crate::error::{KernelError, KernelResult};
use crate::plugin::{PanelManifest, Plugin, PluginManifest, PluginState, PluginStatus};

struct PluginEntry {
    plugin: Box<dyn Plugin>,
    manifest: PluginManifest,
    state: PluginState,
}

/// The microkernel: it owns plugins and the [`KernelCtx`], and does nothing else.
///
/// All real functionality lives in plugins. The kernel's entire job is to order
/// them by their declared dependencies, drive `register` -> `start` -> `stop`, and
/// hold the facilities they share.
///
/// Non-RT: `boot`/`shutdown` run on the main thread.
#[derive(Default)]
pub struct Kernel {
    plugins: Vec<PluginEntry>,
    ctx: KernelCtx,
    /// Indices into `plugins`, in dependency order. Filled by [`Kernel::boot`].
    boot_order: Vec<usize>,
    booted: bool,
}

impl Kernel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a plugin. Order of `add_plugin` calls does not matter -- dependency
    /// order is derived from manifests at boot.
    ///
    /// # Errors
    /// [`KernelError::AddAfterBoot`] if the kernel already booted.
    pub fn add_plugin(&mut self, plugin: Box<dyn Plugin>) -> KernelResult<()> {
        let manifest = plugin.manifest();
        if self.booted {
            return Err(KernelError::AddAfterBoot(manifest.id.to_owned()));
        }
        self.plugins.push(PluginEntry {
            plugin,
            manifest,
            state: PluginState::Registered,
        });
        Ok(())
    }

    /// Topologically sort plugins, then `register()` all of them, then `start()`
    /// all of them -- both passes in dependency order.
    ///
    /// Two passes rather than one so that a plugin's `start()` can rely on *every*
    /// service existing, not just those of its declared dependencies. Dependency
    /// order within each pass still guarantees a plugin sees its own dependencies
    /// fully initialised.
    ///
    /// # Errors
    /// Fails fast, before calling into any plugin, on a duplicate id, a missing
    /// dependency, or a cycle -- a half-booted kernel is worse than no kernel.
    pub fn boot(&mut self) -> KernelResult<()> {
        if self.booted {
            return Err(KernelError::AlreadyBooted);
        }
        let order = topo_order(&self.plugins)?;

        for &i in &order {
            // Disjoint field borrows: `plugins[i]` and `ctx` are separate fields.
            let entry = &mut self.plugins[i];
            log::debug!("registering plugin '{}'", entry.manifest.id);
            entry.plugin.register(&mut self.ctx);
        }
        for &i in &order {
            let entry = &mut self.plugins[i];
            log::debug!("starting plugin '{}'", entry.manifest.id);
            entry.plugin.start(&self.ctx);
            entry.state = PluginState::Started;
        }

        self.boot_order = order;
        self.booted = true;
        Ok(())
    }

    /// Stop plugins in reverse boot order, so each one's dependencies are still
    /// alive while it tears down.
    ///
    /// Idempotent: already-stopped plugins are skipped. Plugins that never started
    /// (i.e. `boot()` was never called) are left alone.
    pub fn shutdown(&mut self) {
        for &i in self.boot_order.clone().iter().rev() {
            let entry = &mut self.plugins[i];
            if entry.state == PluginState::Stopped {
                continue;
            }
            log::debug!("stopping plugin '{}'", entry.manifest.id);
            entry.plugin.stop();
            entry.state = PluginState::Stopped;
        }
    }

    /// Status of every plugin, in `add_plugin` order (stable for the UI list).
    pub fn plugin_statuses(&self) -> Vec<PluginStatus> {
        self.plugins
            .iter()
            .map(|e| PluginStatus {
                id: e.manifest.id.to_owned(),
                name: e.manifest.name.to_owned(),
                state: e.state,
            })
            .collect()
    }

    /// Plugin ids in dependency order. Empty before [`boot`](Self::boot).
    pub fn boot_order(&self) -> Vec<&'static str> {
        self.boot_order
            .iter()
            .map(|&i| self.plugins[i].manifest.id)
            .collect()
    }

    pub fn has_booted(&self) -> bool {
        self.booted
    }

    /// Shared kernel facilities and collected contributions.
    pub fn ctx(&self) -> &KernelCtx {
        &self.ctx
    }

    /// Panels contributed by plugins during `register`.
    pub fn ui_panels(&self) -> &[PanelManifest] {
        &self.ctx.ui_panels
    }
}

/// Kahn's algorithm over manifest dependencies.
///
/// Seeded and processed in `add_plugin` order, so the resulting order is
/// deterministic across runs -- important for reproducible boot logs and tests.
fn topo_order(entries: &[PluginEntry]) -> KernelResult<Vec<usize>> {
    let n = entries.len();

    let mut index_of: HashMap<&'static str, usize> = HashMap::with_capacity(n);
    for (i, entry) in entries.iter().enumerate() {
        if index_of.insert(entry.manifest.id, i).is_some() {
            return Err(KernelError::DuplicatePluginId(entry.manifest.id.to_owned()));
        }
    }

    // `in_degree[i]` = unsatisfied deps of i; `dependents[d]` = who waits on d.
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, entry) in entries.iter().enumerate() {
        for dep in entry.manifest.deps {
            let Some(&d) = index_of.get(dep) else {
                return Err(KernelError::MissingDependency {
                    plugin: entry.manifest.id.to_owned(),
                    dependency: (*dep).to_owned(),
                });
            };
            dependents[d].push(i);
            in_degree[i] += 1;
        }
    }

    let mut ready: VecDeque<usize> = (0..n).filter(|i| in_degree[*i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(i) = ready.pop_front() {
        order.push(i);
        for &dependent in &dependents[i] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                ready.push_back(dependent);
            }
        }
    }

    if order.len() != n {
        // Whatever still has unsatisfied deps is part of, or downstream of, a cycle.
        // A plugin depending on itself lands here too.
        let mut plugins: Vec<String> = in_degree
            .iter()
            .enumerate()
            .filter(|(_, &deg)| deg > 0)
            .map(|(i, _)| entries[i].manifest.id.to_owned())
            .collect();
        plugins.sort();
        return Err(KernelError::DependencyCycle { plugins });
    }

    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use mydaw_rt_core::{ProcessCtx, RtNode, RtNodeFactory};
    use parking_lot::Mutex;

    type Log = Arc<Mutex<Vec<String>>>;

    /// Test plugin that appends every lifecycle call to a shared log and
    /// contributes one panel named after itself.
    struct Recorder {
        manifest: PluginManifest,
        log: Log,
    }

    impl Recorder {
        fn boxed(id: &'static str, deps: &'static [&'static str], log: &Log) -> Box<dyn Plugin> {
            Box::new(Self {
                manifest: PluginManifest::with_deps(id, id, deps),
                log: Arc::clone(log),
            })
        }
    }

    impl Plugin for Recorder {
        fn manifest(&self) -> PluginManifest {
            self.manifest
        }
        fn register(&mut self, ctx: &mut KernelCtx) {
            self.log
                .lock()
                .push(format!("register:{}", self.manifest.id));
            ctx.contribute_panel(PanelManifest::new(
                format!("panel.{}", self.manifest.id),
                self.manifest.name,
                "KernelStatusPanel",
            ));
        }
        fn start(&mut self, _ctx: &KernelCtx) {
            self.log.lock().push(format!("start:{}", self.manifest.id));
        }
        fn stop(&mut self) {
            self.log.lock().push(format!("stop:{}", self.manifest.id));
        }
    }

    fn kernel_with(plugins: Vec<Box<dyn Plugin>>) -> Kernel {
        let mut k = Kernel::new();
        for p in plugins {
            k.add_plugin(p).unwrap();
        }
        k
    }

    #[test]
    fn topological_order_is_respected_for_register_and_start() {
        let log: Log = Arc::default();
        // Added in reverse dependency order on purpose: c -> b -> a.
        let mut k = kernel_with(vec![
            Recorder::boxed("c", &["b"], &log),
            Recorder::boxed("b", &["a"], &log),
            Recorder::boxed("a", &[], &log),
        ]);

        k.boot().unwrap();

        assert_eq!(k.boot_order(), vec!["a", "b", "c"]);
        assert_eq!(
            *log.lock(),
            vec![
                "register:a",
                "register:b",
                "register:c",
                "start:a",
                "start:b",
                "start:c",
            ]
        );
    }

    #[test]
    fn shutdown_stops_in_reverse_order() {
        let log: Log = Arc::default();
        let mut k = kernel_with(vec![
            Recorder::boxed("a", &[], &log),
            Recorder::boxed("b", &["a"], &log),
            Recorder::boxed("c", &["a"], &log),
        ]);
        k.boot().unwrap();
        log.lock().clear();

        k.shutdown();
        assert_eq!(*log.lock(), vec!["stop:c", "stop:b", "stop:a"]);

        // Idempotent: a second shutdown must not re-stop anything.
        log.lock().clear();
        k.shutdown();
        assert!(log.lock().is_empty());
    }

    #[test]
    fn diamond_dependencies_order_correctly() {
        let log: Log = Arc::default();
        //   base <- left, right <- top
        let mut k = kernel_with(vec![
            Recorder::boxed("top", &["left", "right"], &log),
            Recorder::boxed("left", &["base"], &log),
            Recorder::boxed("right", &["base"], &log),
            Recorder::boxed("base", &[], &log),
        ]);
        k.boot().unwrap();
        let order = k.boot_order();
        let pos = |id: &str| order.iter().position(|x| *x == id).unwrap();
        assert!(pos("base") < pos("left"));
        assert!(pos("base") < pos("right"));
        assert!(pos("left") < pos("top"));
        assert!(pos("right") < pos("top"));
    }

    #[test]
    fn missing_dependency_is_reported() {
        let log: Log = Arc::default();
        let mut k = kernel_with(vec![Recorder::boxed("a", &["nope"], &log)]);
        let err = k.boot().unwrap_err();
        assert_eq!(
            err,
            KernelError::MissingDependency {
                plugin: "a".to_owned(),
                dependency: "nope".to_owned(),
            }
        );
        // Fail fast: nothing may have been registered or started.
        assert!(log.lock().is_empty());
        assert!(!k.has_booted());
    }

    #[test]
    fn cycle_is_detected() {
        let log: Log = Arc::default();
        let mut k = kernel_with(vec![
            Recorder::boxed("a", &["c"], &log),
            Recorder::boxed("b", &["a"], &log),
            Recorder::boxed("c", &["b"], &log),
        ]);
        let err = k.boot().unwrap_err();
        assert_eq!(
            err,
            KernelError::DependencyCycle {
                plugins: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            }
        );
        assert!(log.lock().is_empty());
    }

    #[test]
    fn self_dependency_is_a_cycle() {
        let log: Log = Arc::default();
        let mut k = kernel_with(vec![Recorder::boxed("a", &["a"], &log)]);
        assert!(matches!(
            k.boot().unwrap_err(),
            KernelError::DependencyCycle { .. }
        ));
    }

    #[test]
    fn duplicate_plugin_id_is_rejected() {
        let log: Log = Arc::default();
        let mut k = kernel_with(vec![
            Recorder::boxed("dup", &[], &log),
            Recorder::boxed("dup", &[], &log),
        ]);
        assert_eq!(
            k.boot().unwrap_err(),
            KernelError::DuplicatePluginId("dup".to_owned())
        );
    }

    #[test]
    fn boot_twice_is_an_error_and_add_after_boot_is_rejected() {
        let log: Log = Arc::default();
        let mut k = kernel_with(vec![Recorder::boxed("a", &[], &log)]);
        k.boot().unwrap();
        assert_eq!(k.boot().unwrap_err(), KernelError::AlreadyBooted);
        assert_eq!(
            k.add_plugin(Recorder::boxed("b", &[], &log)).unwrap_err(),
            KernelError::AddAfterBoot("b".to_owned())
        );
    }

    #[test]
    fn statuses_track_lifecycle() {
        let log: Log = Arc::default();
        let mut k = kernel_with(vec![
            Recorder::boxed("a", &[], &log),
            Recorder::boxed("b", &["a"], &log),
        ]);

        let before = k.plugin_statuses();
        assert!(before.iter().all(|s| s.state == PluginState::Registered));

        k.boot().unwrap();
        let running = k.plugin_statuses();
        assert_eq!(running.len(), 2);
        // Reported in add order, not boot order.
        assert_eq!(running[0].id, "a");
        assert_eq!(running[1].id, "b");
        assert!(running.iter().all(|s| s.state == PluginState::Started));

        k.shutdown();
        assert!(k
            .plugin_statuses()
            .iter()
            .all(|s| s.state == PluginState::Stopped));
    }

    #[test]
    fn panel_contributions_are_collected_in_dependency_order() {
        let log: Log = Arc::default();
        let mut k = kernel_with(vec![
            Recorder::boxed("second", &["first"], &log),
            Recorder::boxed("first", &[], &log),
        ]);
        k.boot().unwrap();

        let ids: Vec<&str> = k.ui_panels().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["panel.first", "panel.second"]);
        assert_eq!(k.ui_panels()[0].component, "KernelStatusPanel");
        assert_eq!(k.ui_panels()[0].title, "first");
    }

    // --- service + contribution wiring through the real lifecycle ---

    struct DeviceService {
        name: &'static str,
    }

    struct SilenceNode;
    impl RtNode for SilenceNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            ctx.buffers.silence();
        }
    }

    struct SilenceFactory;
    impl RtNodeFactory for SilenceFactory {
        fn type_id(&self) -> &'static str {
            "mydaw.silence"
        }
        fn create(&self, _sample_rate: f64, _max_block: usize) -> Box<dyn RtNode> {
            Box::new(SilenceNode)
        }
    }

    /// Provides a service and an RT node factory in `register`.
    struct Provider;
    impl Plugin for Provider {
        fn manifest(&self) -> PluginManifest {
            PluginManifest::new("provider", "Provider")
        }
        fn register(&mut self, ctx: &mut KernelCtx) {
            ctx.services
                .provide(Arc::new(DeviceService { name: "WASAPI" }))
                .unwrap();
            ctx.register_rt_node_factory(Box::new(SilenceFactory));
        }
        fn start(&mut self, _ctx: &KernelCtx) {}
        fn stop(&mut self) {}
    }

    /// Resolves the dependency's service in `start`, proving ordering works.
    struct Consumer {
        seen: Arc<Mutex<Option<&'static str>>>,
    }
    impl Plugin for Consumer {
        fn manifest(&self) -> PluginManifest {
            PluginManifest::with_deps("consumer", "Consumer", &["provider"])
        }
        fn register(&mut self, _ctx: &mut KernelCtx) {}
        fn start(&mut self, ctx: &KernelCtx) {
            let svc = ctx.services.get::<DeviceService>();
            *self.seen.lock() = svc.map(|s| s.name);
        }
        fn stop(&mut self) {}
    }

    #[test]
    fn dependency_services_are_available_to_dependents() {
        let seen: Arc<Mutex<Option<&'static str>>> = Arc::default();
        let mut k = Kernel::new();
        // Consumer added first to prove ordering comes from manifests, not order.
        k.add_plugin(Box::new(Consumer {
            seen: Arc::clone(&seen),
        }))
        .unwrap();
        k.add_plugin(Box::new(Provider)).unwrap();
        k.boot().unwrap();

        assert_eq!(*seen.lock(), Some("WASAPI"));
    }

    #[test]
    fn rt_node_factories_are_collected_and_resolvable() {
        let mut k = Kernel::new();
        k.add_plugin(Box::new(Provider)).unwrap();
        k.boot().unwrap();

        assert_eq!(k.ctx().rt_node_factories.len(), 1);
        assert_eq!(k.ctx().rt_node_factories[0].0, "mydaw.silence");
        let factory = k
            .ctx()
            .rt_node_factory("mydaw.silence")
            .expect("factory should resolve by id");
        let _node = factory.create(48_000.0, 512);
        assert!(k.ctx().rt_node_factory("mydaw.nonexistent").is_none());
    }
}
