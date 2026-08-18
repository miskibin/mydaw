//! End-to-end hosting tests against the workspace's own CLAP plugin.
//!
//! `test-fixtures/clap-gain` is built as a `cdylib` and loaded here through the
//! real OS loader, the real `clap_entry`, the real factory and the real
//! `clap_plugin->process()`. Nothing is mocked: if these pass, MYDAW hosts CLAP.
//!
//! The fixture multiplies every sample by exactly 0.5, so the assertions are
//! equalities rather than "the level went down a bit".
//!
//! Every test that needs the bundle calls [`fixture`]; if the `cdylib` is
//! missing, the test prints a skip message and returns instead of failing. That
//! is not a licence to let it rot: `test_util::the_workspace_fixture_is_findable_from_the_test_binary`
//! fails loudly if the normal `cargo test --workspace` build stops producing it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use mydaw_fixture_clap_gain::{GAIN, PLUGIN_ID, PLUGIN_NAME, PLUGIN_VENDOR};
use mydaw_plugin_host_clap::hosting::{ClapHostError, LoadedClapPlugin};
use mydaw_plugin_host_clap::test_util::{
    missing_fixture_message, stage_fixture_bundle, FIXTURE_BUNDLE_NAME,
};
use mydaw_plugin_host_clap::ClapHostService;
use mydaw_rt_core::{AudioBuffers, ProcessCtx, RtNode, TransportSnapshot};

const SAMPLE_RATE: f64 = 48_000.0;
const MAX_BLOCK: usize = 256;

/// Self-deleting scratch directory (same three lines as the unit tests; a
/// `tempfile` dependency for this would be heavier than the code).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-clap-host-{tag}-{}-{unique}",
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

/// A scratch directory containing exactly one bundle: the fixture.
///
/// Returns `None` (after printing why) when the fixture was not built, so the
/// caller can skip rather than fail.
fn fixture(tag: &str) -> Option<(TempDir, PathBuf)> {
    let dir = TempDir::new(tag);
    match stage_fixture_bundle(dir.path()) {
        Some(bundle) => Some((dir, bundle)),
        None => {
            eprintln!("{}", missing_fixture_message());
            None
        }
    }
}

/// A 440 Hz sine at the test-tone's amplitude, written into both channels.
fn write_sine(buffers: &mut AudioBuffers, frames: usize, start_frame: u64, amplitude: f32) {
    let increment = 440.0 / SAMPLE_RATE;
    let [left, right] = buffers.as_mut_slices();
    for (index, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let phase = (start_frame as f64 + index as f64) * increment;
        let sample = (phase * std::f64::consts::TAU).sin() as f32 * amplitude;
        *l = sample;
        *r = sample;
    }
    let _ = frames;
}

/// Run one block through `node`, returning both channels of the result.
fn render(node: &mut dyn RtNode, buffers: &mut AudioBuffers, frames: usize) -> [Vec<f32>; 2] {
    let mut ctx = ProcessCtx::new(buffers, SAMPLE_RATE, frames, TransportSnapshot::default());
    node.process(&mut ctx);
    let [left, right] = buffers.as_slices();
    [left.to_vec(), right.to_vec()]
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

#[test]
fn the_scanner_finds_and_describes_the_staged_fixture() {
    let Some((dir, bundle)) = fixture("scan") else {
        return;
    };
    assert_eq!(bundle.file_name().unwrap(), FIXTURE_BUNDLE_NAME);

    let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);
    let found = service.scan();

    assert_eq!(found.len(), 1, "one bundle, one plugin: {found:?}");
    let plugin = &found[0];
    assert_eq!(plugin.id, PLUGIN_ID);
    assert_eq!(plugin.name, PLUGIN_NAME);
    assert_eq!(plugin.vendor, PLUGIN_VENDOR);
    assert!(!plugin.version.is_empty());
    assert_eq!(plugin.path, bundle);
}

#[test]
fn a_scanned_fixture_can_be_instantiated_by_id() {
    let Some((dir, _bundle)) = fixture("by-id") else {
        return;
    };
    let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);

    let plugin = service
        .instantiate_by_id(PLUGIN_ID)
        .expect("the staged fixture must instantiate");
    assert_eq!(plugin.info().id, PLUGIN_ID);
    assert!(!plugin.is_active(), "loading must not activate");
}

// ---------------------------------------------------------------------------
// Hosting: the actual spike
// ---------------------------------------------------------------------------

#[test]
fn the_fixture_reports_one_stereo_port_in_each_direction() {
    let Some((_dir, bundle)) = fixture("layout") else {
        return;
    };
    let plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let layout = plugin.port_layout();

    assert_eq!(layout.input_ports, 1);
    assert_eq!(layout.output_ports, 1);
    assert_eq!(layout.main_input_channels, 2);
    assert_eq!(layout.main_output_channels, 2);
    assert!(layout.is_stereo_effect());
}

#[test]
fn a_sine_processed_through_the_fixture_comes_out_exactly_halved() {
    let Some((_dir, bundle)) = fixture("roundtrip") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");
    assert!(plugin.is_active(), "create_node activates the instance");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    write_sine(&mut buffers, MAX_BLOCK, 0, 0.2);
    let expected: Vec<f32> = buffers.as_slices()[0].iter().map(|s| s * GAIN).collect();

    let [left, right] = render(&mut node, &mut buffers, MAX_BLOCK);

    assert_eq!(left.len(), MAX_BLOCK);
    for (index, (actual, expected)) in left.iter().zip(&expected).enumerate() {
        assert!(
            (actual - expected).abs() <= 1e-7,
            "frame {index}: expected {expected}, got {actual}"
        );
    }
    assert_eq!(left, right, "the fixture treats both channels identically");
    assert!(
        left.iter().all(|s| s.is_finite()),
        "no NaN or infinity may reach the device"
    );
    assert!(
        left.iter().any(|s| s.abs() > 0.01),
        "the signal must survive, not merely be finite"
    );

    assert_eq!(node.stats().process_calls(), 1);
    assert_eq!(node.stats().process_errors(), 0);
}

#[test]
fn silence_in_is_silence_out() {
    let Some((_dir, bundle)) = fixture("silence") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();
    let [left, right] = render(&mut node, &mut buffers, 64);

    assert!(left[..64].iter().all(|s| *s == 0.0), "{:?}", &left[..8]);
    assert!(right[..64].iter().all(|s| *s == 0.0));
}

#[test]
fn repeated_blocks_reuse_the_same_buffers_and_give_the_same_answer() {
    let Some((_dir, bundle)) = fixture("repeat") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    write_sine(&mut buffers, MAX_BLOCK, 0, 0.2);
    let first = render(&mut node, &mut buffers, MAX_BLOCK);

    // Same input again: the fixture is stateless, so the output must be identical
    // bit for bit. A node that reallocated or re-activated between blocks would
    // still pass this, which is why `rt_alloc.rs` checks the allocator directly --
    // this test covers determinism, that one covers RT safety.
    write_sine(&mut buffers, MAX_BLOCK, 0, 0.2);
    let second = render(&mut node, &mut buffers, MAX_BLOCK);

    assert_eq!(first, second);
    assert_eq!(node.stats().process_calls(), 2);
    assert_eq!(node.stats().process_errors(), 0);
}

#[test]
fn a_short_block_processes_only_its_own_frames() {
    let Some((_dir, bundle)) = fixture("short-block") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    for channel in buffers.as_mut_slices() {
        channel.fill(0.8);
    }

    let frames = 17;
    {
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SAMPLE_RATE,
            frames,
            TransportSnapshot::default(),
        );
        node.process(&mut ctx);
    }

    buffers.set_frames(MAX_BLOCK);
    let left = buffers.as_slices()[0];
    assert!(
        left[..frames].iter().all(|s| (*s - 0.4).abs() < 1e-7),
        "processed region should be halved: {:?}",
        &left[..4]
    );
    assert!(
        left[frames..].iter().all(|s| (*s - 0.8).abs() < 1e-7),
        "frames beyond the block must not be touched"
    );
}

#[test]
fn a_block_larger_than_the_activation_size_is_silenced_not_grown() {
    let Some((_dir, bundle)) = fixture("oversized") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    // Activated for 32 frames; the graph will hand it 64.
    let mut node = plugin.create_node(SAMPLE_RATE, 32).expect("activate");

    let mut buffers = AudioBuffers::new(64);
    for channel in buffers.as_mut_slices() {
        channel.fill(0.8);
    }
    let [left, _right] = render(&mut node, &mut buffers, 64);

    assert!(
        left[..32].iter().all(|s| (*s - 0.4).abs() < 1e-7),
        "the part it can process is processed"
    );
    assert!(
        left[32..].iter().all(|s| *s == 0.0),
        "the rest is silenced, never left as stale samples"
    );
    assert_eq!(node.stats().truncated_blocks(), 1);
}

// ---------------------------------------------------------------------------
// Lifecycle and teardown
// ---------------------------------------------------------------------------

#[test]
fn dropping_a_node_hands_the_processor_back_for_main_thread_deactivation() {
    let Some((_dir, bundle)) = fixture("teardown") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");

    let node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");
    assert!(plugin.is_active());

    // Dropping the node must NOT deactivate: that is a main-thread call, and the
    // node is dropped by the basedrop collector thread in the real engine.
    drop(node);
    assert!(
        plugin.is_active(),
        "the node must not deactivate the plugin from its own drop"
    );

    assert_eq!(plugin.collect_retired(), 1);
    assert!(
        !plugin.is_active(),
        "the owner deactivates on the main thread"
    );
}

#[test]
fn a_node_dropped_on_another_thread_still_deactivates_on_the_owner() {
    let Some((_dir, bundle)) = fixture("cross-thread") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    // Exactly what the basedrop collector thread does to a retired graph.
    std::thread::spawn(move || drop(node)).join().unwrap();

    assert!(
        plugin.shutdown(),
        "shutdown waits for the retired processor"
    );
    assert!(!plugin.is_active());
}

#[test]
fn the_plugin_can_be_reactivated_after_a_node_is_retired() {
    let Some((_dir, bundle)) = fixture("reactivate") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");

    let node = plugin.create_node(SAMPLE_RATE, MAX_BLOCK).expect("first");
    drop(node);
    // `create_node` collects retired processors first, so a graph rebuild works
    // without the caller having to sequence the teardown by hand.
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("second activation after the first node retired");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    for channel in buffers.as_mut_slices() {
        channel.fill(1.0);
    }
    let [left, _] = render(&mut node, &mut buffers, MAX_BLOCK);
    assert!(left.iter().all(|s| (*s - 0.5).abs() < 1e-7));
}

#[test]
fn a_second_node_while_one_is_live_is_refused_rather_than_racing() {
    let Some((_dir, bundle)) = fixture("double-activate") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let _node = plugin.create_node(SAMPLE_RATE, MAX_BLOCK).expect("first");

    let err = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect_err("CLAP forbids activating an already-active instance");
    assert!(matches!(err, ClapHostError::Activate { .. }), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Hostile / malformed inputs
// ---------------------------------------------------------------------------

#[test]
fn asking_a_real_bundle_for_a_plugin_id_it_does_not_have_is_an_error() {
    let Some((_dir, bundle)) = fixture("wrong-id") else {
        return;
    };
    let err = LoadedClapPlugin::load(&bundle, "com.example.definitely-not-in-there")
        .expect_err("the fixture exports exactly one plugin, and that is not it");
    assert!(
        matches!(err, ClapHostError::PluginNotFound { .. }),
        "got {err:?}"
    );
}

#[test]
fn a_nonsensical_audio_configuration_is_rejected_before_it_reaches_the_plugin() {
    let Some((_dir, bundle)) = fixture("bad-config") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");

    for (sample_rate, max_block) in [
        (SAMPLE_RATE, 0),
        (0.0, MAX_BLOCK),
        (-48_000.0, MAX_BLOCK),
        (f64::NAN, MAX_BLOCK),
        (f64::INFINITY, MAX_BLOCK),
    ] {
        let err = plugin
            .create_node(sample_rate, max_block)
            .expect_err("clack panics on a zero block size; we must not reach it");
        assert!(
            matches!(err, ClapHostError::InvalidAudioConfiguration { .. }),
            "sample_rate={sample_rate} max_block={max_block} gave {err:?}"
        );
        assert!(!plugin.is_active(), "a rejected config must not activate");
    }
}

#[test]
fn a_directory_that_does_not_exist_scans_to_nothing() {
    let missing = std::env::temp_dir().join("mydaw-clap-host-missing-root-8b3e");
    assert!(!missing.exists());
    let service = ClapHostService::with_search_paths([missing]);
    assert!(service.scan().is_empty());
    assert!(service.cached_scan().is_empty());
}

#[test]
fn a_bundle_that_is_not_a_shared_library_does_not_hide_the_fixture() {
    let Some((dir, _bundle)) = fixture("mixed") else {
        return;
    };
    std::fs::write(dir.path().join("garbage.clap"), b"not a library").unwrap();

    let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);
    let found = service.scan();
    assert_eq!(found.len(), 1, "the broken bundle is skipped: {found:?}");
    assert_eq!(found[0].id, PLUGIN_ID);
}
