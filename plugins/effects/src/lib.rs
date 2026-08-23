//! `mydaw-plugin-effects` -- the mixer effects that ship inside the DAW.
//!
//! Five internal [`RtNode`](mydaw_rt_core::RtNode)s that can go on any track's
//! insert chain or on the master bus, driven by the parameter system in
//! [`mydaw_rt_core::params`].
//!
//! | Node | `type_id` | What it is |
//! | --- | --- | --- |
//! | [`EqNode`](eq::EqNode) | `effects.eq` | Highpass, low shelf, two bells, high shelf. RBJ biquads in `f64`. |
//! | [`CompressorNode`](compressor::CompressorNode) | `effects.compressor` | Feed-forward, log-domain, peak/RMS, soft knee, sidechain highpass. |
//! | [`DelayNode`](delay::DelayNode) | `effects.delay` | Tempo-synced or free, in-loop damping, ping-pong, one-knob ducking. |
//! | [`SaturatorNode`](saturator::SaturatorNode) | `effects.saturator` | Tape/tube/clip shapers, 4x oversampled, drive-compensated. |
//! | [`LimiterNode`](limiter::LimiterNode) | `effects.limiter` | Stereo-linked peak limiter with lookahead. |
//! | [`ReverbNode`](reverb::ReverbNode) | `effects.reverb` | Algorithmic room: decay, size, damp, mix. |
//!
//! # Five properties all of them hold
//!
//! **Transparent when fresh.** A freshly inserted effect is a null-test-clean
//! pass-through at its defaults. Users complain when a "default" carries
//! leftovers, and an effect that changes the sound before you touch it makes
//! every A/B afterwards dishonest.
//!
//! **Click-free bypass.** Every one carries a `bypass` parameter driving a 10 ms
//! equal-gain crossfade, and is **bit-transparent** once the fade has settled --
//! see [`effect::Bypass`]. The VST3 specification puts this on the plugin, and
//! the failure mode is well attested: "horrible click sounds when
//! bypassing/unbypassing".
//!
//! **No allocation on the audio thread.** Every buffer, delay line, filter
//! design and compensation table is built in the factory. `tests/rt_alloc.rs`
//! measures this with a counting global allocator rather than asserting it by
//! inspection.
//!
//! **Bit-determinism.** The same input and the same parameters produce the same
//! samples, exactly. `tests/determinism.rs` checks it, and it is what makes every
//! spectral assertion in this crate a number rather than a tolerance of a
//! tolerance.
//!
//! **Parameters that ramp, switches that do not.** Every continuous parameter is
//! smoothed across block boundaries (the named tell for getting that wrong is
//! "the bigger your buffer, the worse it is"); every discrete one lands on the
//! block boundary, because "you can't smooth discrete values, it just doesn't
//! work".
//!
//! The evidence behind every range, curve and default is in
//! `docs/superpowers/specs/2026-08-19-effects-ux-research.md`.
//!
//! # Using it
//! ```no_run
//! use mydaw_kernel::Kernel;
//! use mydaw_plugin_effects::{EffectRegistry, EffectsPlugin};
//!
//! let mut kernel = Kernel::new();
//! kernel.add_plugin(Box::new(EffectsPlugin::new())).unwrap();
//! kernel.boot().unwrap();
//!
//! let registry = kernel.ctx().services.get::<EffectRegistry>().unwrap();
//! let params = registry.default_params("effects.eq").unwrap();
//! params.set_by_id("low.gain", 6.0);
//! let node = registry.create("effects.eq", 48_000.0, 512, params).unwrap();
//! ```

pub mod biquad;
pub mod compressor;
pub mod delay;
pub mod dsp;
pub mod effect;
pub mod eq;
pub mod limiter;
pub mod oversample;
pub mod reverb;
pub mod saturator;
pub mod test_support;
pub mod utility;

pub use compressor::{CompressorFactory, CompressorNode};
pub use delay::{DelayFactory, DelayNode};
pub use effect::{Bypass, BypassState, BYPASS_PARAM, MIX_PARAM};
pub use eq::{EqFactory, EqNode};
pub use limiter::{LimiterFactory, LimiterNode};
pub use reverb::{ReverbFactory, ReverbNode};
pub use saturator::{SaturatorFactory, SaturatorNode, Shaper};
pub use utility::{UtilityFactory, UtilityNode};

use std::sync::Arc;

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};
use mydaw_rt_core::{ParamBlock, ParamDef, ParamNodeFactory, ReadoutDef, RtNode};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "effects";

/// Display name in the plugin list.
pub const PLUGIN_NAME: &str = "Effects";

/// What the shell needs to know about one internal effect to offer it.
///
/// Three strings and two static slices. The slices are the whole point: a shell
/// can lay out a knob panel and a meter panel for an effect **before** an
/// instance of it exists, which is what makes "choose an effect" a menu rather
/// than a build-and-inspect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectInfo {
    /// The node factory's `type_id`, e.g. `"effects.eq"`. Stable across builds
    /// because project files reference it.
    pub type_id: &'static str,
    /// Name for a menu.
    pub name: &'static str,
    /// One line for a tooltip.
    pub description: &'static str,
    /// Everything this effect exposes, in the order a panel should lay it out.
    pub params: &'static [ParamDef],
    /// Everything it publishes back. Empty for most.
    pub readouts: &'static [ReadoutDef],
}

/// The internal effects, by `type_id`.
///
/// The same shape as
/// [`InstrumentRegistry`](mydaw_plugin_instruments::InstrumentRegistry) and for
/// the same reasons: the shell has to answer "what can I insert?" and "make me
/// one of those" from a string that came out of a webview, and neither the
/// kernel's factory list nor a hard-coded list in the shell can answer both.
pub struct EffectRegistry {
    entries: Vec<(EffectInfo, Arc<dyn ParamNodeFactory>)>,
}

impl Default for EffectRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectRegistry {
    /// The registry every [`EffectsPlugin`] publishes.
    ///
    /// The order is the order a menu should offer them: the two everybody
    /// reaches for first, then the two that colour, then the one that fixes.
    pub fn new() -> Self {
        Self {
            entries: vec![
                Self::entry(
                    eq::NODE_TYPE_ID,
                    eq::DISPLAY_NAME,
                    eq::DESCRIPTION,
                    Arc::new(EqFactory::new()),
                ),
                Self::entry(
                    compressor::NODE_TYPE_ID,
                    compressor::DISPLAY_NAME,
                    compressor::DESCRIPTION,
                    Arc::new(CompressorFactory::new()),
                ),
                Self::entry(
                    delay::NODE_TYPE_ID,
                    delay::DISPLAY_NAME,
                    delay::DESCRIPTION,
                    Arc::new(DelayFactory::new()),
                ),
                Self::entry(
                    saturator::NODE_TYPE_ID,
                    saturator::DISPLAY_NAME,
                    saturator::DESCRIPTION,
                    Arc::new(SaturatorFactory::new()),
                ),
                Self::entry(
                    limiter::NODE_TYPE_ID,
                    limiter::DISPLAY_NAME,
                    limiter::DESCRIPTION,
                    Arc::new(LimiterFactory::new()),
                ),
                Self::entry(
                    reverb::NODE_TYPE_ID,
                    reverb::DISPLAY_NAME,
                    reverb::DESCRIPTION,
                    Arc::new(ReverbFactory::new()),
                ),
                Self::entry(
                    utility::NODE_TYPE_ID,
                    utility::DISPLAY_NAME,
                    utility::DESCRIPTION,
                    Arc::new(UtilityFactory::new()),
                ),
            ],
        }
    }

    fn entry(
        type_id: &'static str,
        name: &'static str,
        description: &'static str,
        factory: Arc<dyn ParamNodeFactory>,
    ) -> (EffectInfo, Arc<dyn ParamNodeFactory>) {
        (
            EffectInfo {
                type_id,
                name,
                description,
                params: factory.params(),
                readouts: factory.readouts(),
            },
            factory,
        )
    }

    /// Every internal effect, in menu order.
    pub fn effects(&self) -> Vec<EffectInfo> {
        self.entries.iter().map(|(info, _)| *info).collect()
    }

    /// Whether `type_id` names an internal effect.
    pub fn contains(&self, type_id: &str) -> bool {
        self.entries.iter().any(|(info, _)| info.type_id == type_id)
    }

    /// Description of one effect.
    pub fn info(&self, type_id: &str) -> Option<EffectInfo> {
        self.entries
            .iter()
            .find(|(info, _)| info.type_id == type_id)
            .map(|(info, _)| *info)
    }

    /// A parameter block at `type_id`'s defaults.
    ///
    /// The shell owns this and keeps it across graph rebuilds; see
    /// [`ParamBlock`]. **Non-RT**: allocates.
    pub fn default_params(&self, type_id: &str) -> Option<Arc<ParamBlock>> {
        self.entries
            .iter()
            .find(|(info, _)| info.type_id == type_id)
            .map(|(_, factory)| factory.default_params())
    }

    /// Instantiate an effect against a block the caller owns. **Non-RT.**
    ///
    /// `None` for a `type_id` this registry does not know, which is a caller
    /// error the caller can report; a registry that silently substituted a
    /// different effect would be far worse.
    pub fn create(
        &self,
        type_id: &str,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
    ) -> Option<Box<dyn RtNode>> {
        self.entries
            .iter()
            .find(|(info, _)| info.type_id == type_id)
            .map(|(_, factory)| factory.create_with_params(sample_rate, max_block, params))
    }

    /// Same as [`create`](Self::create), handing an effect the per-insert
    /// routing state the shell owns alongside its parameters.
    ///
    /// Two effects want something that is not a knob: the EQ its hold-to-solo
    /// band, and the compressor its key source and the bus that carries the key
    /// signals. Everything else ignores both.
    pub fn create_in_chain(
        &self,
        type_id: &str,
        sample_rate: f64,
        max_block: usize,
        params: Arc<ParamBlock>,
        listen: Arc<std::sync::atomic::AtomicI32>,
        sidechain: Arc<std::sync::atomic::AtomicI32>,
        keys: Option<Arc<mydaw_rt_core::SidechainBus>>,
    ) -> Option<Box<dyn RtNode>> {
        if type_id == eq::NODE_TYPE_ID {
            Some(EqFactory::new().create_with_listen(sample_rate, max_block, params, listen))
        } else if type_id == compressor::NODE_TYPE_ID {
            Some(CompressorFactory::new().create_with_key(
                sample_rate,
                max_block,
                params,
                sidechain,
                keys,
            ))
        } else {
            self.create(type_id, sample_rate, max_block, params)
        }
    }
}

impl std::fmt::Debug for EffectRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectRegistry")
            .field(
                "effects",
                &self
                    .entries
                    .iter()
                    .map(|(info, _)| info.type_id)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Contributes the five effect node factories plus the [`EffectRegistry`]
/// service.
///
/// No panels: an effect's editor belongs to whatever draws the mixer, and the
/// parameter metadata this publishes is what it draws from.
#[derive(Debug, Default)]
pub struct EffectsPlugin;

impl EffectsPlugin {
    pub const fn new() -> Self {
        Self
    }
}

impl Plugin for EffectsPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, PLUGIN_NAME)
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        // Registered individually as well as through the registry: a project
        // file names a `type_id`, and the graph builder resolves it against the
        // kernel's factory list without knowing which plugin contributed it.
        ctx.register_rt_node_factory(Box::new(EqFactory::new()));
        ctx.register_rt_node_factory(Box::new(CompressorFactory::new()));
        ctx.register_rt_node_factory(Box::new(DelayFactory::new()));
        ctx.register_rt_node_factory(Box::new(SaturatorFactory::new()));
        ctx.register_rt_node_factory(Box::new(LimiterFactory::new()));
        ctx.register_rt_node_factory(Box::new(ReverbFactory::new()));
        ctx.register_rt_node_factory(Box::new(UtilityFactory::new()));
        let _ = ctx.services.provide(Arc::new(EffectRegistry::new()));
    }

    fn start(&mut self, _ctx: &KernelCtx) {}

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;

    fn booted() -> Kernel {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(EffectsPlugin::new())).unwrap();
        kernel.boot().unwrap();
        kernel
    }

    #[test]
    fn every_factory_is_registered_under_its_type_id() {
        let kernel = booted();
        for info in EffectRegistry::new().effects() {
            let factory = kernel
                .ctx()
                .rt_node_factory(info.type_id)
                .unwrap_or_else(|| panic!("'{}' should be contributed", info.type_id));
            assert_eq!(factory.type_id(), info.type_id);
        }
        assert!(kernel
            .ctx()
            .rt_node_factory("effects.not-an-effect")
            .is_none());
    }

    #[test]
    fn the_registry_is_a_service_and_lists_all_five_in_menu_order() {
        let kernel = booted();
        let registry = kernel
            .ctx()
            .services
            .get::<EffectRegistry>()
            .expect("the registry must be a service");
        let ids: Vec<&str> = registry.effects().iter().map(|i| i.type_id).collect();
        assert_eq!(
            ids,
            vec![
                "effects.eq",
                "effects.compressor",
                "effects.delay",
                "effects.saturator",
                "effects.limiter",
                "effects.reverb",
                "effects.utility",
            ]
        );
    }

    #[test]
    fn every_effect_declares_a_bypass_and_publishes_its_metadata() {
        let registry = EffectRegistry::new();
        for info in registry.effects() {
            assert!(!info.name.is_empty());
            assert!(!info.description.is_empty());
            assert!(
                info.params.iter().any(|p| p.id == BYPASS_PARAM),
                "{} has no bypass",
                info.type_id
            );
            // The bypass is a switch, in every one of them.
            let bypass = info.params.iter().find(|p| p.id == BYPASS_PARAM).unwrap();
            assert!(bypass.is_switch());
            assert_eq!(bypass.default, 0.0, "{} ships bypassed", info.type_id);

            // Ids are unique, or `index_of` would resolve two knobs to one.
            let mut ids: Vec<&str> = info.params.iter().map(|p| p.id).collect();
            ids.sort_unstable();
            let count = ids.len();
            ids.dedup();
            assert_eq!(ids.len(), count, "{} has a duplicate id", info.type_id);

            // Every default is inside its own range, and every log parameter
            // has a positive minimum -- a logarithmic axis has no room for zero.
            for param in info.params {
                assert_eq!(
                    param.sanitise(param.default),
                    param.default,
                    "{}: '{}' default is outside its own range",
                    info.type_id,
                    param.id
                );
                if param.curve == mydaw_rt_core::ParamCurve::Log {
                    assert!(
                        param.min > 0.0,
                        "{}: '{}' is log with min 0",
                        info.type_id,
                        param.id
                    );
                }
            }
        }
    }

    #[test]
    fn a_block_from_the_registry_matches_the_factory_that_will_read_it() {
        let registry = EffectRegistry::new();
        for info in registry.effects() {
            let params = registry.default_params(info.type_id).expect("a block");
            assert_eq!(params.len(), info.params.len());
            assert_eq!(params.readout_defs().len(), info.readouts.len());
            assert!(params.is_default());
            // Building against it must not panic -- `index_of` is a hard error
            // on a typo, so this is the test that catches one.
            assert!(registry
                .create(info.type_id, 48_000.0, 256, params)
                .is_some());
        }
        assert!(registry
            .create(
                "effects.not-an-effect",
                48_000.0,
                256,
                Arc::new(ParamBlock::new(&[]))
            )
            .is_none());
    }

    #[test]
    fn the_type_ids_a_project_file_records_are_the_ones_this_build_answers_to() {
        // Renaming any of these invalidates saved projects.
        assert_eq!(PLUGIN_ID, "effects");
        assert_eq!(eq::NODE_TYPE_ID, "effects.eq");
        assert_eq!(compressor::NODE_TYPE_ID, "effects.compressor");
        assert_eq!(delay::NODE_TYPE_ID, "effects.delay");
        assert_eq!(saturator::NODE_TYPE_ID, "effects.saturator");
        assert_eq!(limiter::NODE_TYPE_ID, "effects.limiter");
        assert_eq!(reverb::NODE_TYPE_ID, "effects.reverb");
        assert_eq!(utility::NODE_TYPE_ID, "effects.utility");
    }
}
