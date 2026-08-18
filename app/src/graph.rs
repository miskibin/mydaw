//! The Phase 1 audio graph: a source -- the test tone, or a CLAP instrument
//! played from the keyboard -- optionally through a CLAP effect.
//!
//! # Two shapes, and why the gate only belongs to one
//!
//! ```text
//!   default:      test-tone -> [effect] -> TransportGate
//!   instrument:   instrument -> [effect]
//! ```
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
//! So the instrument chain has no gate. Live notes are gated by their own
//! note-offs, which is the correct mechanism. When the sequencer arrives it will
//! gate *pattern playback* on the transport (no transport, no scheduled events)
//! while leaving live input alone; that is a property of the event source, not
//! of the audio chain, so it belongs there and not here.

use std::path::PathBuf;

use mydaw_plugin_audio_io::{AudioIoError, AudioIoService};
use mydaw_plugin_host_clap::{ClapHostService, LoadedClapPlugin};
use mydaw_plugin_test_tone::TestToneFactory;
use mydaw_rt_core::{GraphSpec, ProcessCtx, RtNode, RtNodeFactory};

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

/// Silences the bus while the transport is stopped.
///
/// `TestToneNode` is a free-running oscillator: it writes a sine every block
/// regardless of transport state, which is right for a DSP node and wrong for a
/// DAW that is not playing. Rather than teach the oscillator about the transport
/// (Phase 2 gives nodes proper gain/automation), the shell appends this gate to
/// the end of the tone chain, so Play/Stop actually start and stop the sound.
///
/// Deliberately **not** used in the instrument chain; see the module docs.
///
/// # RT safety
/// Reads one `bool` from the per-block transport snapshot and fills pre-allocated
/// slices. No allocation, no locking, no syscalls.
pub struct TransportGate;

impl RtNode for TransportGate {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        if ctx.transport.playing {
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

/// Build the engine graph.
///
/// With an `instrument`: `instrument -> [effect]`, no gate -- an instrument is
/// silent until it is played, and live playing must work with the transport
/// stopped. Without one: `test-tone -> [effect] -> transport gate`, where the
/// gate stays last so that Stop silences whatever the effect produced, including
/// a reverb tail.
///
/// The effect sits after the source either way, because that is where an insert
/// lives.
///
/// Non-RT: node construction allocates, which is exactly why it happens here and
/// not on the audio thread.
pub fn engine_graph(
    factory: &TestToneFactory,
    instrument: Option<Box<dyn RtNode>>,
    effect: Option<Box<dyn RtNode>>,
    sample_rate: f64,
    max_block: usize,
) -> GraphSpec {
    let gated = instrument.is_none();
    let source = instrument.unwrap_or_else(|| factory.create(sample_rate, max_block));

    let mut spec = GraphSpec::new().with(source);
    if let Some(effect) = effect {
        spec.push(effect);
    }
    if gated {
        spec.push(Box::new(TransportGate));
    }
    spec
}

/// The default graph: `test-tone -> [effect] -> transport gate`.
pub fn tone_graph(
    factory: &TestToneFactory,
    effect: Option<Box<dyn RtNode>>,
    sample_rate: f64,
    max_block: usize,
) -> GraphSpec {
    engine_graph(factory, None, effect, sample_rate, max_block)
}

/// Compile and publish [`engine_graph`] to the running audio thread.
///
/// # Errors
/// [`AudioIoError::NotRunning`] when no stream is open (a machine with no output
/// device still boots; it just has nothing to install into).
pub fn install_engine_graph(
    audio: &AudioIoService,
    factory: &TestToneFactory,
    instrument: Option<Box<dyn RtNode>>,
    effect: Option<Box<dyn RtNode>>,
) -> Result<(), AudioIoError> {
    let info = audio.stream_info().ok_or(AudioIoError::NotRunning)?;
    audio.install_graph(engine_graph(
        factory,
        instrument,
        effect,
        f64::from(info.sample_rate),
        info.max_block,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_rt_core::{AudioBuffers, TransportSnapshot};

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
        nodes.push(Box::new(TransportGate));

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
    fn the_default_graph_is_tone_then_gate_and_grows_by_one_with_an_effect() {
        assert_eq!(
            tone_graph(&TestToneFactory::new(), None, SR, BLOCK).len(),
            2
        );
        assert_eq!(
            tone_graph(
                &TestToneFactory::new(),
                Some(Box::new(HalveNode)),
                SR,
                BLOCK
            )
            .len(),
            3
        );
    }

    #[test]
    fn an_instrument_replaces_the_tone_and_drops_the_gate() {
        let factory = TestToneFactory::new();

        // Instrument alone: one node, and it is *not* the gate.
        let spec = engine_graph(&factory, Some(Box::new(SilentNode)), None, SR, BLOCK);
        assert_eq!(spec.len(), 1, "no tone, no gate -- just the instrument");

        // Instrument plus an insert.
        let spec = engine_graph(
            &factory,
            Some(Box::new(SilentNode)),
            Some(Box::new(HalveNode)),
            SR,
            BLOCK,
        );
        assert_eq!(spec.len(), 2);
    }

    #[test]
    fn an_instrument_sounds_with_the_transport_stopped() {
        // The whole point of dropping the gate. A stopped transport must not
        // silence a live instrument, or a user could not audition one without
        // pressing Play first.
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
            "the instrument chain has no gate to silence it"
        );

        // And the tone chain still behaves the old way, for contrast.
        assert!(render(false, None).iter().all(|s| *s == 0.0));
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
