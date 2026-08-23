//! `mydaw-plugin-instruments` -- the instruments that ship inside the DAW.
//!
//! Step 2 gave the engine a note path and proved it against a CLAP fixture.
//! This crate is what makes that path *useful* without any third-party plugin
//! installed: an [`RtNode`](mydaw_rt_core::RtNode) that reads
//! [`ProcessCtx::events`](mydaw_rt_core::ProcessCtx) and makes a sound.
//!
//! | Node | `type_id` | What it is |
//! | --- | --- | --- |
//! | [`PolySynthNode`] | `poly-synth` | Eight voices: three analog oscillators (shape, coarse, fine, mix) into an ADSR and a resonant filter with envelope. |
//!
//! # Where the drums went
//! There were five synthesised drums and a kit node here. They are gone: the
//! sampled `808-kit` is the same machine, recorded rather than approximated, and
//! keeping both meant the browser offered two kicks with nothing to say which
//! one to reach for. A project file naming one of the old ids now fails to load
//! and says what replaced it, which is better than opening silently with a
//! different sound.
//!
//! # Three properties it holds
//!
//! **No assets.** Everything is generated from oscillators. Nothing to ship,
//! nothing to load, nothing to fail at startup, and no licence to track -- which
//! is what makes this the honest fallback when the sampled instruments are not
//! downloaded.
//!
//! **Bit-determinism.** The same note events at the same offsets produce the
//! same samples, exactly. That is not an aesthetic preference: it is what makes
//! `tests/determinism.rs` a real test, what makes the spectral assertions in
//! `tests/spectral.rs` stable numbers rather than tolerances-of-tolerances, and
//! what will make a rendered project file reproducible when there is one. The
//! only randomness in the crate is [`dsp::Noise`], and every voice re-seeds it
//! from a constant on every trigger.
//!
//! **No allocation on the audio thread.** The node is a fixed-size struct;
//! everything that costs -- an `exp`, a `powf`, a filter coefficient -- is
//! computed in the factory, off the RT thread. `tests/rt_alloc.rs` measures this
//! with a counting global allocator over event-heavy blocks rather than
//! asserting it by inspection.
//!
//! # Knobs, and which of these have them
//! The synth declares [`ParamDef`]s and is built against a `ParamBlock` the
//! shell owns -- the same parameter system the effects use, because as far as a
//! knob is concerned an instrument is not a different kind of thing from an
//! effect, and giving instruments a second one would have been two of
//! everything. Automation is not implemented.
//!
//! # Using it
//! ```no_run
//! use mydaw_kernel::Kernel;
//! use mydaw_plugin_instruments::{InstrumentRegistry, InstrumentsPlugin};
//!
//! let mut kernel = Kernel::new();
//! kernel.add_plugin(Box::new(InstrumentsPlugin::new())).unwrap();
//! kernel.boot().unwrap();
//!
//! let registry = kernel.ctx().services.get::<InstrumentRegistry>().unwrap();
//! let node = registry.create("poly-synth", 48_000.0, 512).unwrap();
//! ```

pub mod dsp;
pub mod poly_synth;
pub mod test_support;

pub use dsp::{key_to_hz, Noise, OnePole};
pub use poly_synth::{PolySynthFactory, PolySynthNode, VOICES};

use std::sync::Arc;

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};
use mydaw_rt_core::{ParamDef, ParamNodeFactory, RtNode, RtNodeFactory};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "instruments";

/// Display name in the plugin list.
pub const PLUGIN_NAME: &str = "Instruments";

/// What the shell needs to know about one built-in instrument to offer it.
///
/// Deliberately three strings and nothing else. The moment this grows a
/// parameter list or an icon it stops being a description and starts being a
/// second copy of the instrument, which is the way plugin registries usually go
/// wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstrumentInfo {
    /// The node factory's `type_id`, e.g. `"poly-synth"`. Stable across builds.
    pub type_id: &'static str,
    /// Name for a menu.
    pub name: &'static str,
    /// One line for a tooltip or a status bar.
    pub description: &'static str,
}

/// The knobs an instrument declares, from its `type_id` alone.
///
/// A free function as well as a registry method because the *project* has to
/// answer this -- a track owns its instrument's parameter block, and a project
/// file is loaded on a thread that has no registry, no audio device and no
/// graph. The declaration is `&'static` data on the node type, so there is
/// nothing a registry instance could add to the answer.
pub fn param_defs(type_id: &str) -> &'static [ParamDef] {
    match type_id {
        poly_synth::NODE_TYPE_ID => poly_synth::PARAMS,
        _ => &[],
    }
}

/// The built-in instruments, by `type_id`.
///
/// # Why a service and not just two factories
/// The shell has to answer "what can I switch to?" and "make me one of those"
/// from a string that came out of a webview. Walking the kernel's factory list
/// would answer the second question but not the first -- a factory knows its
/// `type_id` and nothing a menu can show -- and hard-coding the list in the
/// shell would put the names of this crate's nodes in a file that does not
/// depend on them being right.
pub struct InstrumentRegistry {
    entries: Vec<(InstrumentInfo, Arc<dyn RtNodeFactory>)>,
    /// The subset of them that declare knobs, by `type_id`.
    ///
    /// A second list rather than a wider entry, because a `dyn RtNodeFactory`
    /// cannot be asked whether it is also a `dyn ParamNodeFactory`.
    tweakable: Vec<(&'static str, Arc<dyn ParamNodeFactory>)>,
}

impl Default for InstrumentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InstrumentRegistry {
    /// The registry every [`InstrumentsPlugin`] publishes.
    pub fn new() -> Self {
        let entries: Vec<(InstrumentInfo, Arc<dyn RtNodeFactory>)> = vec![(
            InstrumentInfo {
                type_id: poly_synth::NODE_TYPE_ID,
                name: poly_synth::DISPLAY_NAME,
                description: "Eight voices: two oscillators with detune, a sub, \
                              an ADSR and a resonant filter with its own envelope",
            },
            Arc::new(PolySynthFactory::new()),
        )];
        Self {
            entries,
            tweakable: vec![(
                poly_synth::NODE_TYPE_ID,
                Arc::new(PolySynthFactory::new()) as Arc<dyn ParamNodeFactory>,
            )],
        }
    }

    /// What `type_id` exposes as knobs. Empty for an instrument with none.
    pub fn param_defs(&self, type_id: &str) -> &'static [ParamDef] {
        self.tweakable
            .iter()
            .find(|(id, _)| *id == type_id)
            .map_or(&[][..], |(_, factory)| factory.params())
    }

    /// The same answer without a registry to hand. See [`param_defs`].
    pub fn defs_for(type_id: &str) -> &'static [ParamDef] {
        param_defs(type_id)
    }

    /// Instantiate an instrument against a parameter block the caller owns.
    ///
    /// Falls back to [`create`](Self::create) for an instrument with no
    /// parameters, so a caller does not have to ask which kind it has.
    ///
    /// **Non-RT**: allocates.
    pub fn create_with_params(
        &self,
        type_id: &str,
        sample_rate: f64,
        max_block: usize,
        params: Arc<mydaw_rt_core::ParamBlock>,
    ) -> Option<Box<dyn RtNode>> {
        match self.tweakable.iter().find(|(id, _)| *id == type_id) {
            Some((_, factory)) => Some(factory.create_with_params(sample_rate, max_block, params)),
            None => self.create(type_id, sample_rate, max_block),
        }
    }

    /// Every built-in instrument, in menu order.
    pub fn instruments(&self) -> Vec<InstrumentInfo> {
        self.entries.iter().map(|(info, _)| *info).collect()
    }

    /// Whether `type_id` names a built-in instrument.
    pub fn contains(&self, type_id: &str) -> bool {
        self.entries.iter().any(|(info, _)| info.type_id == type_id)
    }

    /// Description of one instrument.
    pub fn info(&self, type_id: &str) -> Option<InstrumentInfo> {
        self.entries
            .iter()
            .find(|(info, _)| info.type_id == type_id)
            .map(|(info, _)| *info)
    }

    /// Instantiate an instrument. **Non-RT** -- this is where a node allocates.
    ///
    /// `None` for a `type_id` this registry does not know, which is a caller
    /// error the caller can report; a registry that silently substituted a
    /// different instrument would be far worse.
    pub fn create(
        &self,
        type_id: &str,
        sample_rate: f64,
        max_block: usize,
    ) -> Option<Box<dyn RtNode>> {
        self.entries
            .iter()
            .find(|(info, _)| info.type_id == type_id)
            .map(|(_, factory)| factory.create(sample_rate, max_block))
    }
}

impl std::fmt::Debug for InstrumentRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstrumentRegistry")
            .field(
                "instruments",
                &self
                    .entries
                    .iter()
                    .map(|(info, _)| info.type_id)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Contributes the `poly-synth` node factory, plus the
/// [`InstrumentRegistry`] service. No panels: choosing an instrument is a
/// property of a track, and there are no tracks yet -- the shell's source
/// selector is a step-3 affordance living in the transport bar.
#[derive(Debug, Default)]
pub struct InstrumentsPlugin;

impl InstrumentsPlugin {
    pub const fn new() -> Self {
        Self
    }
}

impl Plugin for InstrumentsPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, PLUGIN_NAME)
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        // Registered individually as well as through the registry: a project
        // file names a `type_id`, and the graph builder resolves it against the
        // kernel's factory list without knowing which plugin contributed it.
        ctx.register_rt_node_factory(Box::new(PolySynthFactory::new()));
        let _ = ctx.services.provide(Arc::new(InstrumentRegistry::new()));
    }

    fn start(&mut self, _ctx: &KernelCtx) {}

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;
    use mydaw_rt_core::NoteEvent;
    use test_support::{peak, render};

    const SR: f64 = 48_000.0;

    fn booted() -> Kernel {
        let mut kernel = Kernel::new();
        kernel
            .add_plugin(Box::new(InstrumentsPlugin::new()))
            .unwrap();
        kernel.boot().unwrap();
        kernel
    }

    #[test]
    fn every_factory_is_registered_under_its_type_id() {
        let kernel = booted();
        let type_id = poly_synth::NODE_TYPE_ID;
        let factory = kernel
            .ctx()
            .rt_node_factory(type_id)
            .unwrap_or_else(|| panic!("'{type_id}' should be contributed"));
        assert_eq!(factory.type_id(), type_id);
        assert!(kernel.ctx().rt_node_factory("no-such-instrument").is_none());
    }

    #[test]
    fn the_registry_is_published_as_a_service_and_lists_every_instrument() {
        let kernel = booted();
        let registry = kernel
            .ctx()
            .services
            .get::<InstrumentRegistry>()
            .expect("the registry must be a service");

        let ids: Vec<&str> = registry.instruments().iter().map(|i| i.type_id).collect();
        assert_eq!(
            ids,
            vec!["poly-synth"],
            "the drums are sampled now; this crate synthesises the synth alone"
        );
        for info in registry.instruments() {
            assert!(!info.name.is_empty());
            assert!(!info.description.is_empty());
            assert!(registry.contains(info.type_id));
            assert_eq!(registry.info(info.type_id), Some(info));
        }
        assert!(
            !registry.contains("test-tone"),
            "the tone is not an instrument"
        );
        assert_eq!(registry.info("test-tone"), None);
    }

    #[test]
    fn the_registry_builds_a_node_that_answers_to_notes() {
        let registry = InstrumentRegistry::new();
        let mut synth = registry.create("poly-synth", SR, 512).expect("built");
        let (out, _) = render(
            synth.as_mut(),
            9_600,
            512,
            &[NoteEvent::note_on(0, 69, 1.0)],
        );
        assert!(peak(&out) > 0.05, "the synth should have made a sound");
    }

    #[test]
    fn an_unknown_type_id_is_reported_rather_than_substituted() {
        let registry = InstrumentRegistry::new();
        assert!(registry.create("grand-piano", SR, 512).is_none());
    }

    #[test]
    fn the_manifest_names_the_plugin_stably() {
        let plugin = InstrumentsPlugin::new();
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
        assert_eq!(PLUGIN_ID, "instruments");
        // Project files reference these; renaming one invalidates saved graphs.
        assert_eq!(poly_synth::NODE_TYPE_ID, "poly-synth");
        assert_eq!(poly_synth::NODE_TYPE_ID, "poly-synth");
    }
}
