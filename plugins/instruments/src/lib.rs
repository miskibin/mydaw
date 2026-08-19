//! `mydaw-plugin-instruments` -- the instruments that ship inside the DAW.
//!
//! Step 2 gave the engine a note path and proved it against a CLAP fixture.
//! This crate is what makes that path *useful* without any third-party plugin
//! installed: two [`RtNode`](mydaw_rt_core::RtNode)s that read
//! [`ProcessCtx::events`](mydaw_rt_core::ProcessCtx) and make a sound.
//!
//! | Node | `type_id` | What it is |
//! | --- | --- | --- |
//! | [`DrumKitNode`] | `drum-kit` | Kick, snare and two hi-hats, synthesised. General MIDI keys 36/38/42/46. |
//! | [`PolySynthNode`] | `poly-synth` | Eight voices of polyBLEP saw + sine sub, ADSR, one-pole lowpass. |
//!
//! # Three properties both of them hold
//!
//! **No assets.** Everything is generated from oscillators and a seeded noise
//! source. Nothing to ship, nothing to load, nothing to fail at startup, and no
//! licence to track.
//!
//! **Bit-determinism.** The same note events at the same offsets produce the
//! same samples, exactly. That is not an aesthetic preference: it is what makes
//! `tests/determinism.rs` a real test, what makes the spectral assertions in
//! `tests/spectral.rs` stable numbers rather than tolerances-of-tolerances, and
//! what will make a rendered project file reproducible when there is one. The
//! only randomness in the crate is [`dsp::Noise`], and every voice re-seeds it
//! from a constant on every trigger.
//!
//! **No allocation on the audio thread.** Both nodes are fixed-size structs;
//! everything that costs -- an `exp`, a `powf`, a filter coefficient -- is
//! computed in the factory, off the RT thread. `tests/rt_alloc.rs` measures this
//! with a counting global allocator over event-heavy blocks rather than
//! asserting it by inspection.
//!
//! # What is deliberately missing
//! Every parameter is a constant. There is no cutoff knob, no tuning, no
//! per-drum level, and no preset. Parameters need a parameter queue, smoothing
//! and automation, which is the mixer phase's work; a knob wired to a field with
//! no smoothing would click, and a knob wired to nothing would be a lie. See
//! `docs/superpowers/specs/2026-08-19-step3-internal-instruments.md`.
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

pub mod drum_kit;
pub mod dsp;
pub mod poly_synth;
pub mod test_support;

pub use drum_kit::{
    Drum, DrumKitFactory, DrumKitNode, CLOSED_HAT_KEY, KICK_KEY, OPEN_HAT_KEY, SNARE_KEY,
};
pub use dsp::{key_to_hz, Noise, OnePole};
pub use poly_synth::{PolySynthFactory, PolySynthNode, VOICES};

use std::sync::Arc;

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};
use mydaw_rt_core::{RtNode, RtNodeFactory};

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
}

impl Default for InstrumentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InstrumentRegistry {
    /// The registry every [`InstrumentsPlugin`] publishes.
    pub fn new() -> Self {
        Self {
            entries: vec![
                (
                    InstrumentInfo {
                        type_id: poly_synth::NODE_TYPE_ID,
                        name: poly_synth::DISPLAY_NAME,
                        description: "Eight-voice saw + sub with an ADSR and a 3 kHz lowpass",
                    },
                    Arc::new(PolySynthFactory::new()),
                ),
                (
                    InstrumentInfo {
                        type_id: drum_kit::NODE_TYPE_ID,
                        name: drum_kit::DISPLAY_NAME,
                        description: "Synthesised kick, snare and hi-hats on GM keys 36/38/42/46",
                    },
                    Arc::new(DrumKitFactory::new()),
                ),
            ],
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

/// Contributes the `poly-synth` and `drum-kit` node factories, plus the
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
        ctx.register_rt_node_factory(Box::new(DrumKitFactory::new()));
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
    fn both_factories_are_registered_under_their_type_ids() {
        let kernel = booted();
        for type_id in [poly_synth::NODE_TYPE_ID, drum_kit::NODE_TYPE_ID] {
            let factory = kernel
                .ctx()
                .rt_node_factory(type_id)
                .unwrap_or_else(|| panic!("'{type_id}' should be contributed"));
            assert_eq!(factory.type_id(), type_id);
        }
        assert!(kernel.ctx().rt_node_factory("no-such-instrument").is_none());
    }

    #[test]
    fn the_registry_is_published_as_a_service_and_lists_both_instruments() {
        let kernel = booted();
        let registry = kernel
            .ctx()
            .services
            .get::<InstrumentRegistry>()
            .expect("the registry must be a service");

        let ids: Vec<&str> = registry.instruments().iter().map(|i| i.type_id).collect();
        assert_eq!(
            ids,
            vec!["poly-synth", "drum-kit"],
            "the synth leads: it is the default source"
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

        let mut drums = registry.create("drum-kit", SR, 512).expect("built");
        let (out, _) = render(
            drums.as_mut(),
            9_600,
            512,
            &[NoteEvent::note_on(0, KICK_KEY, 1.0)],
        );
        assert!(peak(&out) > 0.05, "the kit should have made a sound");
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
        assert_eq!(drum_kit::NODE_TYPE_ID, "drum-kit");
    }
}
