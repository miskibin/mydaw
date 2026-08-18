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

use mydaw_fixture_clap_gain::synth::{
    key_to_hz, ATTACK_MS, PLUGIN_ID as SYNTH_ID, PLUGIN_NAME as SYNTH_NAME, RELEASE_MS, VOICES,
    VOICE_GAIN,
};
use mydaw_fixture_clap_gain::{GAIN, PLUGIN_ID, PLUGIN_NAME, PLUGIN_VENDOR};
use mydaw_plugin_host_clap::hosting::{ClapHostError, LoadedClapPlugin};
use mydaw_plugin_host_clap::test_util::{
    missing_fixture_message, stage_fixture_bundle, FIXTURE_BUNDLE_NAME,
};
use mydaw_plugin_host_clap::ClapHostService;
use mydaw_rt_core::{AudioBuffers, NoteEvent, ProcessCtx, RtNode, TransportSnapshot};

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

    // One bundle, two plugins -- the layout every real vendor ships, and the
    // reason `ClapPluginInfo::path` is not a unique key.
    assert_eq!(found.len(), 2, "one bundle, two plugins: {found:?}");
    assert!(found.iter().all(|p| p.path == bundle));
    assert!(found.iter().all(|p| p.vendor == PLUGIN_VENDOR));
    assert!(found.iter().all(|p| !p.version.is_empty()));

    let gain = found
        .iter()
        .find(|p| p.id == PLUGIN_ID)
        .expect("the gain fixture must be listed");
    assert_eq!(gain.name, PLUGIN_NAME);

    let synth = found
        .iter()
        .find(|p| p.id == SYNTH_ID)
        .expect("the synth fixture must be listed");
    assert_eq!(synth.name, SYNTH_NAME);
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
    assert_eq!(
        found.len(),
        2,
        "the broken bundle is skipped, the good one keeps both its plugins: {found:?}"
    );
    assert!(found.iter().any(|p| p.id == PLUGIN_ID));
    assert!(found.iter().any(|p| p.id == SYNTH_ID));
}

// ---------------------------------------------------------------------------
// Note dispatch (step 2)
// ---------------------------------------------------------------------------
//
// The claim under test: a `NoteEvent` in `ProcessCtx` becomes a CLAP note event
// inside a real plugin, through the real FFI boundary, and the plugin makes the
// sound it was asked for. The synth fixture is deterministic, so these assert
// frequencies and amplitudes rather than "something happened".

/// Render one block with note events attached.
fn render_with_events(
    node: &mut dyn RtNode,
    buffers: &mut AudioBuffers,
    frames: usize,
    events: &[NoteEvent],
) -> [Vec<f32>; 2] {
    let mut ctx = ProcessCtx::with_events(
        buffers,
        SAMPLE_RATE,
        frames,
        TransportSnapshot::default(),
        events,
    );
    node.process(&mut ctx);
    let [left, right] = buffers.as_slices();
    [left.to_vec(), right.to_vec()]
}

/// Largest absolute sample.
fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()))
}

/// Frequency estimated from rising zero crossings. Good enough for one sine,
/// meaningless for a chord -- which is why the chord test measures level.
fn zero_crossing_hz(samples: &[f32]) -> f64 {
    let mut crossings = 0_usize;
    let mut first = None;
    let mut last = 0_usize;
    for (index, pair) in samples.windows(2).enumerate() {
        if pair[0] <= 0.0 && pair[1] > 0.0 {
            if first.is_none() {
                first = Some(index);
            }
            last = index;
            crossings += 1;
        }
    }
    let Some(first) = first.filter(|_| crossings > 1) else {
        return 0.0;
    };
    let span = (last - first) as f64;
    (crossings - 1) as f64 * SAMPLE_RATE / span
}

#[test]
fn the_synth_fixture_reports_an_instrument_layout_with_a_note_input() {
    let Some((_dir, bundle)) = fixture("synth-layout") else {
        return;
    };
    let plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load");
    let layout = plugin.port_layout();

    assert_eq!(layout.input_ports, 0, "an instrument takes no audio in");
    assert_eq!(layout.output_ports, 1);
    assert_eq!(layout.main_output_channels, 2);
    assert_eq!(layout.note_input_ports, 1);
    assert_eq!(layout.note_output_ports, 0);
    assert!(layout.is_instrument());
    assert!(!layout.is_stereo_effect());
    assert!(layout.is_supported(), "the graph must accept an instrument");
    assert!(layout.has_note_input());
    assert!(!layout.has_audio_input());
}

#[test]
fn an_instrument_is_silent_until_it_is_played() {
    let Some((_dir, bundle)) = fixture("synth-silent") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("an instrument must activate");
    assert!(node.delivers_notes());

    // Deliberately noisy input: an instrument must overwrite the bus, not add
    // to it, or a chain would leak whatever was upstream.
    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    write_sine(&mut buffers, MAX_BLOCK, 0, 0.5);

    let [left, right] = render_with_events(&mut node, &mut buffers, MAX_BLOCK, &[]);
    assert_eq!(peak(&left), 0.0, "no notes, no sound");
    assert_eq!(peak(&right), 0.0);
    assert_eq!(node.stats().events_sent(), 0);
    assert_eq!(node.stats().events_dropped(), 0);
    assert_eq!(node.stats().process_errors(), 0);
}

#[test]
fn a_note_on_reaches_the_plugin_and_sounds_at_the_key_frequency() {
    let Some((_dir, bundle)) = fixture("synth-note-on") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();

    // A4 at full velocity, then let it run until the 2 ms attack is over.
    render_with_events(
        &mut node,
        &mut buffers,
        MAX_BLOCK,
        &[NoteEvent::note_on(0, 69, 1.0)],
    );
    assert_eq!(node.stats().events_sent(), 1);
    assert_eq!(node.stats().events_dropped(), 0);

    let mut sustained = Vec::new();
    for _ in 0..16 {
        let [left, right] = render_with_events(&mut node, &mut buffers, MAX_BLOCK, &[]);
        assert_eq!(
            left, right,
            "the synth is mono, fanned out to both channels"
        );
        sustained.extend_from_slice(&left);
    }

    let expected_peak = VOICE_GAIN;
    assert!(
        (peak(&sustained) - expected_peak).abs() < 0.01,
        "velocity 1.0 should peak at {expected_peak}, got {}",
        peak(&sustained)
    );
    let measured = zero_crossing_hz(&sustained);
    assert!(
        (measured - f64::from(key_to_hz(69))).abs() < 1.0,
        "key 69 should sound at 440 Hz, measured {measured:.2} Hz"
    );
    assert_eq!(node.stats().process_errors(), 0);
}

#[test]
fn velocity_scales_the_amplitude_the_plugin_produces() {
    let Some((_dir, bundle)) = fixture("synth-velocity") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();
    render_with_events(
        &mut node,
        &mut buffers,
        MAX_BLOCK,
        &[NoteEvent::note_on(0, 69, 0.5)],
    );

    let mut sustained = Vec::new();
    for _ in 0..8 {
        let [left, _] = render_with_events(&mut node, &mut buffers, MAX_BLOCK, &[]);
        sustained.extend_from_slice(&left);
    }
    let expected = VOICE_GAIN * 0.5;
    assert!(
        (peak(&sustained) - expected).abs() < 0.01,
        "velocity 0.5 should peak at {expected}, got {}",
        peak(&sustained)
    );
}

#[test]
fn the_attack_ramp_starts_from_silence_so_a_note_on_does_not_click() {
    let Some((_dir, bundle)) = fixture("synth-attack") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();
    let [left, _] = render_with_events(
        &mut node,
        &mut buffers,
        MAX_BLOCK,
        &[NoteEvent::note_on(0, 69, 1.0)],
    );

    // A step to full amplitude would put a broadband click into the spectrum,
    // which is exactly what a capture test is trying to measure around.
    assert!(left[0].abs() < 0.01, "the first sample must be near zero");
    let attack_frames = ((ATTACK_MS / 1000.0) * SAMPLE_RATE as f32) as usize;
    assert!(
        peak(&left[..attack_frames.min(left.len())]) < VOICE_GAIN,
        "the ramp must not have finished inside the attack window"
    );
}

#[test]
fn a_note_off_releases_the_voice_to_silence() {
    let Some((_dir, bundle)) = fixture("synth-note-off") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();
    render_with_events(
        &mut node,
        &mut buffers,
        MAX_BLOCK,
        &[NoteEvent::note_on(0, 69, 1.0)],
    );
    for _ in 0..8 {
        render_with_events(&mut node, &mut buffers, MAX_BLOCK, &[]);
    }
    let [held, _] = render_with_events(&mut node, &mut buffers, MAX_BLOCK, &[]);
    assert!(peak(&held) > 0.1, "the note is sounding before the release");

    render_with_events(
        &mut node,
        &mut buffers,
        MAX_BLOCK,
        &[NoteEvent::note_off(0, 69)],
    );

    // 50 ms at 48 kHz is 2400 frames; give it a margin and then demand digital
    // silence, not merely "quieter".
    let release_frames = ((RELEASE_MS / 1000.0) * SAMPLE_RATE as f32) as usize;
    let blocks = release_frames / MAX_BLOCK + 2;
    let mut tail = Vec::new();
    for _ in 0..blocks {
        let [left, _] = render_with_events(&mut node, &mut buffers, MAX_BLOCK, &[]);
        tail.extend_from_slice(&left);
    }
    let [after, _] = render_with_events(&mut node, &mut buffers, MAX_BLOCK, &[]);
    assert_eq!(
        peak(&after),
        0.0,
        "the voice must be free once the release has run out"
    );
    assert!(
        peak(&tail) > 0.0,
        "the release must be a ramp, not an instant cut"
    );
    assert_eq!(node.stats().events_sent(), 2);
}

#[test]
fn a_chord_sounds_every_key_it_was_given() {
    let Some((_dir, bundle)) = fixture("synth-chord") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();
    // A C major triad, arriving in one block like a struck chord.
    let chord = [
        NoteEvent::note_on(0, 60, 1.0),
        NoteEvent::note_on(0, 64, 1.0),
        NoteEvent::note_on(0, 67, 1.0),
    ];
    render_with_events(&mut node, &mut buffers, MAX_BLOCK, &chord);
    assert_eq!(node.stats().events_sent(), 3);

    let mut sustained = Vec::new();
    for _ in 0..16 {
        let [left, _] = render_with_events(&mut node, &mut buffers, MAX_BLOCK, &[]);
        sustained.extend_from_slice(&left);
    }

    // Three independent sines: the sum must be louder than one voice and can
    // never exceed three, which proves all three arrived without needing an FFT
    // in this crate (the shell's capture test does the spectral version).
    let measured = peak(&sustained);
    assert!(
        measured > VOICE_GAIN * 1.5,
        "three voices should be clearly louder than one, got {measured}"
    );
    assert!(
        measured <= VOICE_GAIN * 3.0 + 1e-4,
        "three voices cannot exceed three voices, got {measured}"
    );
    assert!(sustained.iter().all(|s| s.is_finite()));
}

#[test]
fn events_are_placed_at_their_frame_offset_not_at_the_start_of_the_block() {
    let Some((_dir, bundle)) = fixture("synth-offset") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();
    const OFFSET: u32 = 128;
    let [left, _] = render_with_events(
        &mut node,
        &mut buffers,
        MAX_BLOCK,
        &[NoteEvent::note_on(OFFSET, 69, 1.0)],
    );

    // Everything before the offset must be untouched silence. This is the whole
    // difference between a sample-accurate host and one that quantises to the
    // block -- invisible until the sequencer arrives, so worth asserting now.
    assert_eq!(
        peak(&left[..OFFSET as usize]),
        0.0,
        "nothing may sound before the event's frame"
    );
    assert!(
        peak(&left[OFFSET as usize..]) > 0.0,
        "the note must sound from its own frame onwards"
    );
}

#[test]
fn note_events_for_a_plugin_without_a_note_port_are_skipped_silently() {
    let Some((_dir, bundle)) = fixture("gain-notes") else {
        return;
    };
    // The gain fixture declares no note ports at all. Sending it events must be
    // a no-op: not an error, and not a delivery.
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");
    assert!(!node.delivers_notes());

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    write_sine(&mut buffers, MAX_BLOCK, 0, 0.2);
    let expected: Vec<f32> = buffers.as_slices()[0].iter().map(|s| s * GAIN).collect();

    let [left, _] = render_with_events(
        &mut node,
        &mut buffers,
        MAX_BLOCK,
        &[NoteEvent::note_on(0, 69, 1.0), NoteEvent::note_off(0, 69)],
    );

    for (actual, expected) in left.iter().zip(&expected) {
        assert!((actual - expected).abs() <= 1e-7);
    }
    assert_eq!(node.stats().events_sent(), 0, "nothing was delivered");
    assert_eq!(
        node.stats().events_dropped(),
        0,
        "a plugin with no note input is a configuration, not a loss"
    );
    assert_eq!(node.stats().process_errors(), 0);
}

#[test]
fn more_voices_than_the_synth_has_steal_rather_than_stack() {
    let Some((_dir, bundle)) = fixture("synth-steal") else {
        return;
    };
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load");
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();
    // Twelve keys into eight voices.
    let events: Vec<NoteEvent> = (60..72)
        .map(|key| NoteEvent::note_on(0, key, 1.0))
        .collect();
    render_with_events(&mut node, &mut buffers, MAX_BLOCK, &events);
    assert_eq!(node.stats().events_sent(), events.len() as u64);

    let mut sustained = Vec::new();
    for _ in 0..16 {
        let [left, _] = render_with_events(&mut node, &mut buffers, MAX_BLOCK, &[]);
        sustained.extend_from_slice(&left);
    }
    assert!(
        peak(&sustained) <= VOICE_GAIN * VOICES as f32 + 1e-4,
        "no more than {VOICES} voices may ever sound at once, got {}",
        peak(&sustained)
    );
    assert!(sustained.iter().all(|s| s.is_finite()));
}
