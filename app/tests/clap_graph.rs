//! The spike's headline claim, tested: a real CLAP plugin processing audio
//! inside the real RT graph.
//!
//! No device and no webview are involved. The graph is `mydaw-rt-core`'s own --
//! `GraphSpec` -> `GraphHandle::install` -> `RtGraphSlot::poll_swap`/`process`,
//! with `basedrop` retiring the old graph -- so what runs here is the same code
//! the audio callback runs, driven by this thread instead of by cpal.
//!
//! The plugin is the workspace's fixture (`test-fixtures/clap-gain`, a -6.02 dB
//! stereo gain), loaded from disk through the OS loader. If it has not been
//! built, each test prints a skip message and returns; the host-clap unit test
//! `the_workspace_fixture_is_findable_from_the_test_binary` is what makes a
//! permanently-skipping suite impossible to miss.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use mydaw_app::{engine_graph, TransportGate};
use mydaw_fixture_clap_gain::{GAIN, PLUGIN_ID};
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_host_clap::LoadedClapPlugin;
use mydaw_plugin_test_tone::{TestToneFactory, DEFAULT_AMPLITUDE};
use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{
    graph_channel, AudioBuffers, GraphSpec, ProcessCtx, RtNode, RtNodeFactory, TransportSnapshot,
};

const SAMPLE_RATE: f64 = 48_000.0;
const BLOCK: usize = 128;
/// Enough blocks that a stateful bug (phase reset, a node run once) would show.
const BLOCKS: usize = 32;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-app-clap-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stage the fixture, or `None` (with a printed reason) if it was not built.
fn staged(tag: &str) -> Option<(TempDir, PathBuf)> {
    let dir = TempDir::new(tag);
    match stage_fixture_bundle(dir.path()) {
        Some(bundle) => Some((dir, bundle)),
        None => {
            eprintln!("{}", missing_fixture_message());
            None
        }
    }
}

/// Install `spec` into a real graph channel and render `BLOCKS` blocks with the
/// transport running, returning the peak absolute sample seen on the left channel.
///
/// This exercises the lock-free publish path, not just the nodes: the graph is
/// wrapped in `basedrop::Owned`, pushed through the `rtrb` queue, and picked up by
/// the RT-side slot exactly as the audio callback does.
fn render_peak(spec: GraphSpec) -> f32 {
    let mut collector = Collector::new();
    let (mut handle, mut slot) = graph_channel(2, collector.handle());
    handle.install(spec).expect("graph installs");

    let mut buffers = AudioBuffers::new(BLOCK);
    let mut peak = 0.0_f32;
    let mut sample_pos = 0_u64;

    assert!(slot.poll_swap(), "the RT side must pick the graph up");
    for _ in 0..BLOCKS {
        let transport = TransportSnapshot {
            sample_pos,
            playing: true,
            ..TransportSnapshot::default()
        };
        {
            let mut ctx = ProcessCtx::new(&mut buffers, SAMPLE_RATE, BLOCK, transport);
            slot.process(&mut ctx);
        }
        let [left, right] = buffers.as_slices();
        assert_eq!(left, right, "the whole chain is channel-symmetric");
        for sample in left {
            assert!(sample.is_finite(), "a non-finite sample reached the bus");
            peak = peak.max(sample.abs());
        }
        sample_pos += BLOCK as u64;
    }

    // Stand in for the GC thread: retire the graph and actually run the
    // destructors, so a `ClapNode` in it hands its processor back before the
    // caller tries to deactivate. In the app this is `GcThread`'s job.
    drop(slot);
    drop(handle);
    collector.collect();
    assert_eq!(collector.alloc_count(), 0, "the graph was not collected");

    peak
}

#[test]
fn a_clap_effect_in_the_graph_halves_the_test_tone() {
    let Some((_dir, bundle)) = staged("halves") else {
        return;
    };

    let factory = TestToneFactory::new();
    let plain = render_peak(engine_graph(factory.create(SAMPLE_RATE, BLOCK), None, true));

    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("fixture loads");
    let node = plugin.create_node(SAMPLE_RATE, BLOCK).expect("activates");
    let hosted = render_peak(engine_graph(
        factory.create(SAMPLE_RATE, BLOCK),
        Some(Box::new(node)),
        true,
    ));

    // The tone is -14 dBFS; a full 128-frame block at 440 Hz reaches the crest,
    // so the plain peak is the oscillator's amplitude to within a sample.
    assert!(
        (plain - DEFAULT_AMPLITUDE).abs() < 1e-3,
        "tone-only peak should be ~{DEFAULT_AMPLITUDE}, got {plain}"
    );
    assert!(
        (hosted - plain * GAIN).abs() < 1e-6,
        "with the -6 dB fixture inserted the peak should be {}, got {hosted}",
        plain * GAIN
    );

    // -6.02 dB, expressed the way the meter panel will show it.
    let delta_db = 20.0 * f64::from(hosted / plain).log10();
    assert!(
        (delta_db + 6.0206).abs() < 0.01,
        "expected -6.02 dB, got {delta_db:.3} dB"
    );

    assert!(plugin.shutdown(), "the plugin deactivates on this thread");
}

#[test]
fn the_transport_gate_still_wins_with_a_clap_effect_inserted() {
    let Some((_dir, bundle)) = staged("gate") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("fixture loads");
    let node = plugin.create_node(SAMPLE_RATE, BLOCK).expect("activates");

    let mut nodes: Vec<Box<dyn RtNode>> = vec![
        TestToneFactory::new().create(SAMPLE_RATE, BLOCK),
        Box::new(node),
        Box::new(TransportGate::always()),
    ];

    let mut buffers = AudioBuffers::new(BLOCK);
    let mut ctx = ProcessCtx::new(
        &mut buffers,
        SAMPLE_RATE,
        BLOCK,
        TransportSnapshot {
            sample_pos: 0,
            playing: false,
            ..TransportSnapshot::default()
        },
    );
    for node in &mut nodes {
        node.process(&mut ctx);
    }

    assert!(
        buffers.as_slices()[0].iter().all(|s| *s == 0.0),
        "Stop must silence the output even when a plugin is producing signal"
    );

    drop(nodes);
    assert!(plugin.shutdown());
}

#[test]
fn a_graph_holding_a_clap_node_is_retired_by_the_collector_not_the_rt_thread() {
    let Some((_dir, bundle)) = staged("retire") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("fixture loads");
    let node = plugin.create_node(SAMPLE_RATE, BLOCK).expect("activates");

    let mut collector = Collector::new();
    let (mut handle, mut slot) = graph_channel(4, collector.handle());
    let factory = TestToneFactory::new();

    handle
        .install(engine_graph(
            factory.create(SAMPLE_RATE, BLOCK),
            Some(Box::new(node)),
            true,
        ))
        .expect("install");
    slot.poll_swap();

    // Swap in a graph without the plugin, exactly as a user removing an insert
    // would. The old graph -- and the `ClapNode` inside it -- is dropped by the
    // "audio thread" here, which under `basedrop` means *queued*, not destroyed.
    handle
        .install(engine_graph(factory.create(SAMPLE_RATE, BLOCK), None, true))
        .expect("install");
    slot.poll_swap();
    assert!(
        plugin.is_active(),
        "retiring a graph must not deactivate a plugin from the audio thread"
    );
    assert_eq!(
        plugin.collect_retired(),
        0,
        "nothing can have been retired before the collector runs"
    );

    // Now the collector runs the destructor, on a non-RT thread -- which is what
    // sends the audio processor back to the main thread.
    collector.collect();
    assert_eq!(plugin.collect_retired(), 1);
    assert!(!plugin.is_active());

    // The remaining graph still works after its neighbour was torn down.
    let mut buffers = AudioBuffers::new(BLOCK);
    let mut ctx = ProcessCtx::new(
        &mut buffers,
        SAMPLE_RATE,
        BLOCK,
        TransportSnapshot {
            sample_pos: 0,
            playing: true,
            ..TransportSnapshot::default()
        },
    );
    slot.process(&mut ctx);
    assert!(buffers.as_slices()[0].iter().any(|s| s.abs() > 0.01));
}
