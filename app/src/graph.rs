//! The Phase 1 audio graph: a test tone, optionally through a CLAP effect,
//! behind a transport gate.

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

/// Silences the bus while the transport is stopped.
///
/// `TestToneNode` is a free-running oscillator: it writes a sine every block
/// regardless of transport state, which is right for a DSP node and wrong for a
/// DAW that is not playing. Rather than teach the oscillator about the transport
/// (Phase 2 gives nodes proper gain/automation), the shell appends this gate to
/// the end of the chain, so Play/Stop actually start and stop the sound.
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

/// A CLAP effect named by [`CLAP_FX_ENV`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClapEffectSpec {
    pub path: PathBuf,
    pub id: String,
}

/// Why [`CLAP_FX_ENV`] could not be understood. Never fatal: the shell logs it
/// and boots without the effect.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ClapFxEnvError {
    #[error("{CLAP_FX_ENV} must be '<bundle path>;<plugin id>', got '{0}'")]
    Malformed(String),
    #[error("{CLAP_FX_ENV} has an empty {0}")]
    Empty(&'static str),
}

/// Parse the `<path>;<id>` form. Whitespace around either half is trimmed,
/// because shells and `.env` files put it there.
pub fn parse_clap_fx_spec(value: &str) -> Result<ClapEffectSpec, ClapFxEnvError> {
    let Some((path, id)) = value.rsplit_once(';') else {
        return Err(ClapFxEnvError::Malformed(value.to_owned()));
    };
    let (path, id) = (path.trim(), id.trim());
    if path.is_empty() {
        return Err(ClapFxEnvError::Empty("bundle path"));
    }
    if id.is_empty() {
        return Err(ClapFxEnvError::Empty("plugin id"));
    }
    Ok(ClapEffectSpec {
        path: PathBuf::from(path),
        id: id.to_owned(),
    })
}

/// Read [`CLAP_FX_ENV`], returning `None` when it is unset (the normal case).
pub fn clap_fx_spec_from_env() -> Option<Result<ClapEffectSpec, ClapFxEnvError>> {
    let raw = std::env::var(CLAP_FX_ENV).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    Some(parse_clap_fx_spec(&raw))
}

/// Load the CLAP effect requested by the environment, if any.
///
/// Returns `None` for "nothing was asked for" **and** for "it was asked for and
/// failed" -- both are logged, and neither may stop the DAW from booting. A
/// missing or broken plugin is a normal Tuesday; a shell that refuses to start
/// because of one is not.
///
/// The returned [`LoadedClapPlugin`] is not `Send` and owns the plugin instance:
/// keep it on the main thread, and keep it alive for at least as long as any node
/// it creates (see the `hosting` module docs in `mydaw-plugin-host-clap`).
pub fn load_env_clap_effect(clap: &ClapHostService) -> Option<LoadedClapPlugin> {
    let spec = match clap_fx_spec_from_env()? {
        Ok(spec) => spec,
        Err(err) => {
            log::warn!("ignoring {CLAP_FX_ENV}: {err}");
            return None;
        }
    };

    match clap.load(&spec.path, &spec.id) {
        Ok(plugin) => {
            log::info!(
                "CLAP effect loaded: '{}' ({}) from {}",
                plugin.info().name,
                plugin.info().id,
                plugin.info().path.display()
            );
            Some(plugin)
        }
        Err(err) => {
            log::warn!(
                "could not load the CLAP effect from {CLAP_FX_ENV} ({}): {err}",
                spec.path.display()
            );
            None
        }
    }
}

/// Activate `plugin` for this stream and wrap it in an RT node.
///
/// Returns `None` (having logged) when the plugin will not activate -- an
/// instrument rather than an effect, a mono plugin, a plugin that simply says no.
/// The graph is then built without it.
pub fn clap_effect_node(
    plugin: &mut LoadedClapPlugin,
    sample_rate: f64,
    max_block: usize,
) -> Option<Box<dyn RtNode>> {
    match plugin.create_node(sample_rate, max_block) {
        Ok(node) => Some(Box::new(node)),
        Err(err) => {
            log::warn!(
                "CLAP effect '{}' could not be activated ({err}); running without it",
                plugin.info().id
            );
            None
        }
    }
}

/// Build the default graph: `test-tone -> [effect] -> transport gate`.
///
/// The gate stays last so that Stop silences whatever the effect produced,
/// including a reverb tail. The effect sits after the generator because that is
/// where an insert lives.
///
/// Non-RT: node construction allocates, which is exactly why it happens here and
/// not on the audio thread.
pub fn tone_graph(
    factory: &TestToneFactory,
    effect: Option<Box<dyn RtNode>>,
    sample_rate: f64,
    max_block: usize,
) -> GraphSpec {
    let mut spec = GraphSpec::new().with(factory.create(sample_rate, max_block));
    if let Some(effect) = effect {
        spec.push(effect);
    }
    spec.push(Box::new(TransportGate));
    spec
}

/// Compile and publish [`tone_graph`] to the running audio thread.
///
/// # Errors
/// [`AudioIoError::NotRunning`] when no stream is open (a machine with no output
/// device still boots; it just has nothing to install into).
pub fn install_tone_graph(
    audio: &AudioIoService,
    factory: &TestToneFactory,
    effect: Option<Box<dyn RtNode>>,
) -> Result<(), AudioIoError> {
    let info = audio.stream_info().ok_or(AudioIoError::NotRunning)?;
    audio.install_graph(tone_graph(
        factory,
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
    fn the_env_override_parses_a_windows_path_and_a_reverse_dns_id() {
        let spec =
            parse_clap_fx_spec(r"C:\Program Files\Common Files\CLAP\Thing.clap;com.acme.thing")
                .unwrap();
        assert_eq!(
            spec.path,
            PathBuf::from(r"C:\Program Files\Common Files\CLAP\Thing.clap")
        );
        assert_eq!(spec.id, "com.acme.thing");
    }

    #[test]
    fn the_env_override_splits_on_the_last_separator_and_trims() {
        let spec = parse_clap_fx_spec("  /odd;path/a.clap ; org.x.y  ").unwrap();
        assert_eq!(spec.path, PathBuf::from("/odd;path/a.clap"));
        assert_eq!(spec.id, "org.x.y");
    }

    #[test]
    fn a_malformed_env_override_is_a_typed_error_not_a_panic() {
        assert_eq!(
            parse_clap_fx_spec("no-separator-here"),
            Err(ClapFxEnvError::Malformed("no-separator-here".to_owned()))
        );
        assert_eq!(
            parse_clap_fx_spec(";com.acme.thing"),
            Err(ClapFxEnvError::Empty("bundle path"))
        );
        assert_eq!(
            parse_clap_fx_spec("thing.clap;"),
            Err(ClapFxEnvError::Empty("plugin id"))
        );
        assert_eq!(
            parse_clap_fx_spec("thing.clap;   "),
            Err(ClapFxEnvError::Empty("plugin id"))
        );
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
