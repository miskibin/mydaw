//! The Phase 1 audio graph: a swappable source, optionally through a CLAP
//! effect, and a transport gate that only some sources ask for.
//!
//! ```text
//!   SourceSlot -> [CLAP effect] -> TransportGate
//! ```
//!
//! The chain is installed once per stream. What changes at runtime is the node
//! *inside* [`SourceSlot`](crate::source::SourceSlot) -- see `crate::source`
//! for why the swap happens there and not by rebuilding the graph.
//!
//! # Why the gate is conditional rather than absent
//!
//! [`TransportGate`] exists because `TestToneNode` is a free-running
//! oscillator: it makes a sine whether or not the DAW is playing, so the shell
//! has to silence it when the transport is stopped. An instrument has the
//! opposite problem. It is silent until it is *played*, and it is played live,
//! by a human, whose expectation is that pressing a key makes a sound -- exactly
//! as it does in every DAW, with the transport stopped, before anything has been
//! recorded. Gating it on the transport would make the instrument mute unless
//! you pressed Play first, which is not a stricter engine, just a broken one.
//!
//! So an instrument is not gated. Live notes are gated by their own note-offs,
//! which is the correct mechanism. When the sequencer arrives it will gate
//! *pattern playback* on the transport (no transport, no scheduled events)
//! while leaving live input alone; that is a property of the event source, not
//! of the audio chain, so it belongs there and not here.
//!
//! The gate node itself stays **last** in the chain either way, so Stop
//! silences whatever the effect produced -- including a reverb tail -- rather
//! than only the generator. Whether it is armed is a flag the source publishes
//! as it is installed, so the two can never disagree about which source is
//! playing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mydaw_plugin_audio_io::{AudioIoError, AudioIoService};
use mydaw_plugin_host_clap::{ClapHostService, LoadedClapPlugin};
use mydaw_rt_core::{GraphSpec, ProcessCtx, RtNode};

use crate::source::{Source, SourceHandle};

/// Environment variable that inserts a CLAP effect into the default graph.
///
/// Format: `<bundle path>;<CLAP plugin id>`, e.g.
/// `MYDAW_CLAP_FX=D:\build\mydaw-gain.clap;dev.mydaw.fixture.gain`.
/// Split on the **last** `;` so a path containing one still works (a plugin id
/// containing one does not, and CLAP ids are reverse-DNS by convention).
///
/// This is a spike affordance, not a feature: Phase 2 loads effects from the
/// project file and a plugin browser. It exists so the spike can be verified on a
/// real machine without a UI for inserting plugins.
pub const CLAP_FX_ENV: &str = "MYDAW_CLAP_FX";

/// Environment variable that makes a CLAP **instrument** the graph's source,
/// replacing the test tone.
///
/// Same `<bundle path>;<CLAP plugin id>` format as [`CLAP_FX_ENV`], e.g.
/// `MYDAW_CLAP_INSTRUMENT=D:\build\mydaw-gain.clap;dev.mydaw.fixture.synth`.
/// Unset (the normal case) leaves the test-tone graph exactly as it was.
///
/// Also a step-2 affordance rather than a feature: Phase 2 loads instruments
/// from the project file onto tracks.
pub const CLAP_INSTRUMENT_ENV: &str = "MYDAW_CLAP_INSTRUMENT";

/// Silences the bus while the transport is stopped -- when the current source
/// asks to be silenced.
///
/// `TestToneNode` is a free-running oscillator: it writes a sine every block
/// regardless of transport state, which is right for a DSP node and wrong for a
/// DAW that is not playing. Rather than teach the oscillator about the transport
/// (Phase 2 gives nodes proper gain/automation), the shell appends this gate to
/// the end of the chain, so Play/Stop actually start and stop the sound.
///
/// An instrument must *not* be gated (see the module docs), and the source can
/// change while the stream is running, so the decision is a shared flag rather
/// than the gate's presence in the graph. The flag is written by
/// [`SourceSlot`](crate::source::SourceSlot) at the head of the same chain, in
/// the same block, so it always describes the source that just rendered.
///
/// # RT safety
/// Reads one atomic and one `bool` from the per-block transport snapshot, then
/// fills pre-allocated slices. No allocation, no locking, no syscalls.
pub struct TransportGate {
    armed: Arc<AtomicBool>,
}

impl TransportGate {
    /// A gate armed by `armed`, which the source slot publishes.
    pub fn new(armed: Arc<AtomicBool>) -> Self {
        Self { armed }
    }

    /// A gate that is always armed, for chains built by hand (tests, and the
    /// offline renders in `tests/clap_graph.rs`) rather than by a source slot.
    pub fn always() -> Self {
        Self::new(Arc::new(AtomicBool::new(true)))
    }

    /// Whether this gate is currently silencing a stopped transport.
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }
}

impl RtNode for TransportGate {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        if !self.is_armed() || ctx.transport.playing {
            return;
        }
        // `as_mut_slices` is already truncated to the block's frame count.
        for channel in ctx.buffers.as_mut_slices() {
            channel.fill(0.0);
        }
    }
}

/// A CLAP plugin named by an environment variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClapPluginSpec {
    pub path: PathBuf,
    pub id: String,
}

/// Why a `<path>;<id>` environment variable could not be understood. Never
/// fatal: the shell logs it and boots without the plugin.
///
/// Both variants carry the variable's name, because the same parser serves
/// [`CLAP_FX_ENV`] and [`CLAP_INSTRUMENT_ENV`] and a message naming the wrong
/// one would send the reader to the wrong shell command.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ClapPluginEnvError {
    #[error("{var} must be '<bundle path>;<plugin id>', got '{value}'")]
    Malformed { var: &'static str, value: String },
    #[error("{var} has an empty {field}")]
    Empty {
        var: &'static str,
        field: &'static str,
    },
}

/// Parse the `<path>;<id>` form. Whitespace around either half is trimmed,
/// because shells and `.env` files put it there.
pub fn parse_clap_plugin_spec(
    var: &'static str,
    value: &str,
) -> Result<ClapPluginSpec, ClapPluginEnvError> {
    let Some((path, id)) = value.rsplit_once(';') else {
        return Err(ClapPluginEnvError::Malformed {
            var,
            value: value.to_owned(),
        });
    };
    let (path, id) = (path.trim(), id.trim());
    if path.is_empty() {
        return Err(ClapPluginEnvError::Empty {
            var,
            field: "bundle path",
        });
    }
    if id.is_empty() {
        return Err(ClapPluginEnvError::Empty {
            var,
            field: "plugin id",
        });
    }
    Ok(ClapPluginSpec {
        path: PathBuf::from(path),
        id: id.to_owned(),
    })
}

/// Read `var`, returning `None` when it is unset or blank (the normal case).
pub fn clap_spec_from_env(var: &'static str) -> Option<Result<ClapPluginSpec, ClapPluginEnvError>> {
    let raw = std::env::var(var).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    Some(parse_clap_plugin_spec(var, &raw))
}

/// Read [`CLAP_FX_ENV`].
pub fn clap_fx_spec_from_env() -> Option<Result<ClapPluginSpec, ClapPluginEnvError>> {
    clap_spec_from_env(CLAP_FX_ENV)
}

/// Read [`CLAP_INSTRUMENT_ENV`].
pub fn clap_instrument_spec_from_env() -> Option<Result<ClapPluginSpec, ClapPluginEnvError>> {
    clap_spec_from_env(CLAP_INSTRUMENT_ENV)
}

/// Load the CLAP plugin named by `var`, if any.
///
/// Returns `None` for "nothing was asked for" **and** for "it was asked for and
/// failed" -- both are logged, and neither may stop the DAW from booting. A
/// missing or broken plugin is a normal Tuesday; a shell that refuses to start
/// because of one is not.
///
/// The returned [`LoadedClapPlugin`] is not `Send` and owns the plugin instance:
/// keep it on the main thread, and keep it alive for at least as long as any node
/// it creates (see the `hosting` module docs in `mydaw-plugin-host-clap`).
pub fn load_env_clap_plugin(
    clap: &ClapHostService,
    var: &'static str,
    role: &str,
) -> Option<LoadedClapPlugin> {
    let spec = match clap_spec_from_env(var)? {
        Ok(spec) => spec,
        Err(err) => {
            log::warn!("ignoring {var}: {err}");
            return None;
        }
    };

    match clap.load(&spec.path, &spec.id) {
        Ok(plugin) => {
            log::info!(
                "CLAP {role} loaded: '{}' ({}) from {}",
                plugin.info().name,
                plugin.info().id,
                plugin.info().path.display()
            );
            Some(plugin)
        }
        Err(err) => {
            log::warn!(
                "could not load the CLAP {role} from {var} ({}): {err}",
                spec.path.display()
            );
            None
        }
    }
}

/// Load the CLAP effect requested by [`CLAP_FX_ENV`], if any.
pub fn load_env_clap_effect(clap: &ClapHostService) -> Option<LoadedClapPlugin> {
    load_env_clap_plugin(clap, CLAP_FX_ENV, "effect")
}

/// Load the CLAP instrument requested by [`CLAP_INSTRUMENT_ENV`], if any.
pub fn load_env_clap_instrument(clap: &ClapHostService) -> Option<LoadedClapPlugin> {
    load_env_clap_plugin(clap, CLAP_INSTRUMENT_ENV, "instrument")
}

/// Activate `plugin` for this stream and wrap it in an RT node.
///
/// Returns `None` (having logged) when the plugin will not activate -- a mono
/// plugin, a plugin whose ports the graph cannot drive, a plugin that simply
/// says no. The graph is then built without it.
pub fn clap_node(
    plugin: &mut LoadedClapPlugin,
    role: &str,
    sample_rate: f64,
    max_block: usize,
) -> Option<Box<dyn RtNode>> {
    match plugin.create_node(sample_rate, max_block) {
        Ok(node) => Some(Box::new(node)),
        Err(err) => {
            log::warn!(
                "CLAP {role} '{}' could not be activated ({err}); running without it",
                plugin.info().id
            );
            None
        }
    }
}

/// Activate a CLAP effect. See [`clap_node`].
pub fn clap_effect_node(
    plugin: &mut LoadedClapPlugin,
    sample_rate: f64,
    max_block: usize,
) -> Option<Box<dyn RtNode>> {
    clap_node(plugin, "effect", sample_rate, max_block)
}

/// Activate a CLAP instrument. See [`clap_node`].
pub fn clap_instrument_node(
    plugin: &mut LoadedClapPlugin,
    sample_rate: f64,
    max_block: usize,
) -> Option<Box<dyn RtNode>> {
    clap_node(plugin, "instrument", sample_rate, max_block)
}

/// Build a plain serial chain: `source -> [effect] -> [gate]`.
///
/// The effect sits after the source, because that is where an insert lives, and
/// the gate goes last, so Stop silences whatever the effect produced.
///
/// This is the shape the engine runs, minus the source slot: the running engine
/// uses [`install_engine_graph`], and this exists for the offline renders that
/// want a chain without a stream behind it.
///
/// Non-RT: node construction allocates, which is exactly why it happens here and
/// not on the audio thread.
pub fn engine_graph(
    source: Box<dyn RtNode>,
    effect: Option<Box<dyn RtNode>>,
    gated: bool,
) -> GraphSpec {
    let mut spec = GraphSpec::new().with(source);
    if let Some(effect) = effect {
        spec.push(effect);
    }
    if gated {
        spec.push(Box::new(TransportGate::always()));
    }
    spec
}

/// Compile and publish `SourceSlot -> [effect] -> TransportGate` to the running
/// audio thread, and hand `handle` the slot's non-RT end.
///
/// `node` is the source to start on; after this returns, changing the source is
/// [`SourceHandle::install`] and does not touch the graph again -- which is what
/// lets a CLAP effect survive an instrument change (see `crate::source`).
///
/// The gate is always in the chain and armed by the flag the slot publishes, so
/// a later swap to (or from) the test tone arms or disarms it without another
/// graph install.
///
/// # Errors
/// [`AudioIoError::NotRunning`] when no stream is open (a machine with no output
/// device still boots; it just has nothing to install into).
pub fn install_engine_graph(
    audio: &AudioIoService,
    handle: &SourceHandle,
    source: Source,
    node: Box<dyn RtNode>,
    effect: Option<Box<dyn RtNode>>,
) -> Result<(), AudioIoError> {
    // Fails before anything is built when there is no stream, so a headless
    // machine does not pay for a graph nobody can hear.
    let _ = audio.stream_info().ok_or(AudioIoError::NotRunning)?;

    let gate = Arc::new(AtomicBool::new(source.gated()));
    let slot = handle.open(node, source, audio.gc_handle(), Arc::clone(&gate));
    audio.install_graph(source_chain(slot, effect, gate))
}

/// Assemble `SourceSlot -> [effect] -> TransportGate`.
///
/// Split out of [`install_engine_graph`] so the offline tests build the chain
/// the engine actually runs rather than a copy of it that can drift.
pub fn source_chain(
    slot: crate::source::SourceSlot,
    effect: Option<Box<dyn RtNode>>,
    gate: Arc<AtomicBool>,
) -> GraphSpec {
    let mut spec = GraphSpec::new().with(Box::new(slot) as Box<dyn RtNode>);
    if let Some(effect) = effect {
        spec.push(effect);
    }
    spec.push(Box::new(TransportGate::new(gate)));
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_plugin_test_tone::TestToneFactory;
    use mydaw_rt_core::{AudioBuffers, RtNodeFactory, TransportSnapshot};

    const SR: f64 = 48_000.0;
    const BLOCK: usize = 64;

    /// Render one block of `test-tone -> [effect] -> gate` with the given
    /// transport state, returning the left channel.
    fn render(playing: bool, effect: Option<Box<dyn RtNode>>) -> Vec<f32> {
        let factory = TestToneFactory::new();
        let mut nodes: Vec<Box<dyn RtNode>> = vec![factory.create(SR, BLOCK)];
        if let Some(effect) = effect {
            nodes.push(effect);
        }
        nodes.push(Box::new(TransportGate::always()));

        let mut buffers = AudioBuffers::new(BLOCK);
        let transport = TransportSnapshot {
            sample_pos: 0,
            playing,
        };
        let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, transport);
        for node in &mut nodes {
            node.process(&mut ctx);
        }

        buffers.as_slices()[0].to_vec()
    }

    /// Stand-in for a CLAP effect in tests that are about the graph, not CLAP.
    struct HalveNode;
    impl RtNode for HalveNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for channel in ctx.buffers.as_mut_slices() {
                for sample in channel.iter_mut() {
                    *sample *= 0.5;
                }
            }
        }
    }

    /// Stand-in for a CLAP instrument with nothing to play.
    struct SilentNode;
    impl RtNode for SilentNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            ctx.buffers.silence();
        }
    }

    /// Stand-in for a CLAP instrument that has been played.
    struct ConstNode(f32);
    impl RtNode for ConstNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for channel in ctx.buffers.as_mut_slices() {
                channel.fill(self.0);
            }
        }
    }

    #[test]
    fn the_gate_passes_audio_while_playing() {
        let left = render(true, None);
        assert!(
            left.iter().any(|s| s.abs() > 0.01),
            "a 440 Hz tone should be audible while the transport runs"
        );
    }

    #[test]
    fn the_gate_silences_a_stopped_transport() {
        let left = render(false, None);
        assert!(
            left.iter().all(|s| *s == 0.0),
            "nothing may reach the device while the transport is stopped"
        );
    }

    #[test]
    fn an_effect_sits_between_the_generator_and_the_gate() {
        let plain = render(true, None);
        let halved = render(true, Some(Box::new(HalveNode)));

        let plain_peak = plain.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        let halved_peak = halved.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!((halved_peak - plain_peak / 2.0).abs() < 1e-6);

        // And the gate still wins: an insert must not make Stop leak audio.
        assert!(render(false, Some(Box::new(HalveNode)))
            .iter()
            .all(|s| *s == 0.0));
    }

    #[test]
    fn the_tone_chain_is_tone_then_gate_and_grows_by_one_with_an_effect() {
        let tone = || TestToneFactory::new().create(SR, BLOCK);
        assert_eq!(engine_graph(tone(), None, true).len(), 2);
        assert_eq!(
            engine_graph(tone(), Some(Box::new(HalveNode)), true).len(),
            3
        );
    }

    #[test]
    fn an_ungated_chain_is_just_the_source_and_its_insert() {
        // An instrument alone: one node, and it is *not* the gate.
        let spec = engine_graph(Box::new(SilentNode), None, false);
        assert_eq!(spec.len(), 1, "no gate -- just the instrument");

        let spec = engine_graph(Box::new(SilentNode), Some(Box::new(HalveNode)), false);
        assert_eq!(spec.len(), 2);
    }

    #[test]
    fn an_instrument_sounds_with_the_transport_stopped() {
        // The whole point of the gate being conditional. A stopped transport
        // must not silence a live instrument, or a user could not audition one
        // without pressing Play first.
        let mut nodes: Vec<Box<dyn RtNode>> = vec![Box::new(ConstNode(0.3))];
        let mut buffers = AudioBuffers::new(BLOCK);
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                sample_pos: 0,
                playing: false,
            },
        );
        for node in &mut nodes {
            node.process(&mut ctx);
        }
        assert!(
            buffers.as_slices()[0].iter().all(|s| *s == 0.3),
            "an ungated chain has nothing to silence it"
        );

        // And the tone chain still behaves the old way, for contrast.
        assert!(render(false, None).iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_disarmed_gate_passes_a_stopped_transport_through_untouched() {
        // The runtime half of the same decision: one gate node, two behaviours,
        // chosen by the flag the source slot publishes.
        let armed = Arc::new(AtomicBool::new(true));
        let mut nodes: Vec<Box<dyn RtNode>> = vec![
            Box::new(ConstNode(0.3)),
            Box::new(TransportGate::new(Arc::clone(&armed))),
        ];
        let stopped = TransportSnapshot {
            sample_pos: 0,
            playing: false,
        };

        let mut buffers = AudioBuffers::new(BLOCK);
        {
            let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, stopped);
            for node in &mut nodes {
                node.process(&mut ctx);
            }
        }
        assert!(buffers.as_slices()[0].iter().all(|s| *s == 0.0), "armed");

        armed.store(false, Ordering::Release);
        {
            let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, stopped);
            for node in &mut nodes {
                node.process(&mut ctx);
            }
        }
        assert!(
            buffers.as_slices()[0].iter().all(|s| *s == 0.3),
            "a disarmed gate must be transparent"
        );
    }

    #[test]
    fn the_env_override_parses_a_windows_path_and_a_reverse_dns_id() {
        let spec = parse_clap_plugin_spec(
            CLAP_FX_ENV,
            r"C:\Program Files\Common Files\CLAP\Thing.clap;com.acme.thing",
        )
        .unwrap();
        assert_eq!(
            spec.path,
            PathBuf::from(r"C:\Program Files\Common Files\CLAP\Thing.clap")
        );
        assert_eq!(spec.id, "com.acme.thing");
    }

    #[test]
    fn the_env_override_splits_on_the_last_separator_and_trims() {
        let spec =
            parse_clap_plugin_spec(CLAP_INSTRUMENT_ENV, "  /odd;path/a.clap ; org.x.y  ").unwrap();
        assert_eq!(spec.path, PathBuf::from("/odd;path/a.clap"));
        assert_eq!(spec.id, "org.x.y");
    }

    #[test]
    fn a_malformed_env_override_is_a_typed_error_naming_the_variable() {
        assert_eq!(
            parse_clap_plugin_spec(CLAP_FX_ENV, "no-separator-here"),
            Err(ClapPluginEnvError::Malformed {
                var: CLAP_FX_ENV,
                value: "no-separator-here".to_owned()
            })
        );
        assert_eq!(
            parse_clap_plugin_spec(CLAP_FX_ENV, ";com.acme.thing"),
            Err(ClapPluginEnvError::Empty {
                var: CLAP_FX_ENV,
                field: "bundle path"
            })
        );
        assert_eq!(
            parse_clap_plugin_spec(CLAP_INSTRUMENT_ENV, "thing.clap;"),
            Err(ClapPluginEnvError::Empty {
                var: CLAP_INSTRUMENT_ENV,
                field: "plugin id"
            })
        );
        assert_eq!(
            parse_clap_plugin_spec(CLAP_INSTRUMENT_ENV, "thing.clap;   "),
            Err(ClapPluginEnvError::Empty {
                var: CLAP_INSTRUMENT_ENV,
                field: "plugin id"
            })
        );

        // The message must send the reader to the right shell command.
        let message = parse_clap_plugin_spec(CLAP_INSTRUMENT_ENV, "nope")
            .unwrap_err()
            .to_string();
        assert!(message.contains(CLAP_INSTRUMENT_ENV), "{message}");
        assert!(!message.contains(CLAP_FX_ENV), "{message}");
    }

    #[test]
    fn the_two_environment_variables_are_distinct_and_documented() {
        assert_eq!(CLAP_FX_ENV, "MYDAW_CLAP_FX");
        assert_eq!(CLAP_INSTRUMENT_ENV, "MYDAW_CLAP_INSTRUMENT");
        // Neither is set in the test process, so both read as absent.
        assert!(clap_fx_spec_from_env().is_none());
        assert!(clap_instrument_spec_from_env().is_none());
    }

    #[test]
    fn loading_an_effect_that_is_not_there_yields_none_instead_of_failing_boot() {
        // Uses the service's own load path with a bundle that cannot exist, which
        // is the branch `load_env_clap_effect` takes when the env var points at a
        // plugin the user has since uninstalled.
        let service = ClapHostService::with_search_paths([]);
        let missing = std::env::temp_dir().join("mydaw-app-no-such-plugin-4d2a.clap");
        assert!(service.load(&missing, "com.acme.gone").is_err());
    }
}
