//! End-to-end capture through the real RT path, with no audio device.
//!
//! This is the test the whole harness exists for: a real `rt-core` graph
//! (test-tone -> transport gate) is driven through [`run_block`] exactly as the
//! cpal callback drives it, while a [`DebugTapService`] on another thread runs a
//! real [`capture`](DebugTapService::capture) against it. If the arming flag,
//! the ring, the frame alignment, the sample-rate hand-off or the analysis were
//! wrong, the assertions below would be wrong with them.
//!
//! No device is opened, so this runs on a headless CI machine.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mydaw_plugin_audio_io::{run_block, AudioIoStats, RtState, RtStateParams};
use mydaw_plugin_debug_tap::{debug_tap_channel, CaptureError, DebugTapService};
use mydaw_plugin_sequencer::{song_channel, SONG_QUEUE_CAPACITY};
use mydaw_plugin_test_tone::{TestToneNode, DEFAULT_AMPLITUDE, DEFAULT_FREQUENCY_HZ};
use mydaw_rt_core::{
    graph_channel, note_channel, rt_channel, GcThread, GraphSpec, ProcessCtx, RtCommand, RtNode,
    TransportClock,
};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Nothing here sends notes; the engine still needs a queue to drain.
const NOTE_CAPACITY: usize = 16;
const MAX_BLOCK: usize = 512;

/// The shell's transport gate, duplicated here so this test does not depend on
/// the app crate. Silences the bus while the transport is stopped.
struct TransportGate;

impl RtNode for TransportGate {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        if ctx.transport.playing {
            return;
        }
        for channel in ctx.buffers.as_mut_slices() {
            channel.fill(0.0);
        }
    }
}

/// Everything the "audio thread" side of the test owns.
struct Engine {
    state: RtState,
    output: Vec<f32>,
    stats: Arc<AudioIoStats>,
    _gc: GcThread,
    _graph: mydaw_rt_core::GraphHandle,
}

/// Build the RT side and hand back the capture service that watches it.
fn engine(playing: bool) -> (Engine, Arc<DebugTapService>) {
    let gc = GcThread::spawn();
    let (mut graph, slot) = graph_channel(4, gc.handle());
    let (mut commands_tx, commands) = rt_channel::<RtCommand>(16);
    // 0.5 s of stereo audio at 48 kHz is 48 000 samples; give the ring room for
    // more than the capture needs so a scheduling hiccup cannot cost samples.
    let (tap_rt, tap_consumer) = debug_tap_channel(1 << 17);
    let transport = Arc::new(TransportClock::new(SR));
    let stats = Arc::new(AudioIoStats::default());

    graph
        .install(
            GraphSpec::new()
                .with(Box::new(TestToneNode::with_defaults(SR)))
                .with(Box::new(TransportGate)),
        )
        .expect("the graph should install");
    commands_tx
        .push(RtCommand::SetPlaying(playing))
        .expect("the command queue is empty");

    let (_notes_tx, notes) = note_channel(NOTE_CAPACITY);
    let (_song_tx, song) = song_channel(SONG_QUEUE_CAPACITY, gc.handle());
    let state = RtState::new(RtStateParams {
        graph: slot,
        commands,
        notes,
        note_capacity: NOTE_CAPACITY,
        song,
        meters: None,
        debug_tap: Some(tap_rt),
        spectrum: None,
        transport,
        sample_rate: SR,
        out_channels: 2,
        max_block: MAX_BLOCK,
        stats: Arc::clone(&stats),
    });

    (
        Engine {
            state,
            output: vec![0.0; BLOCK * 2],
            stats,
            _gc: gc,
            _graph: graph,
        },
        Arc::new(DebugTapService::new(tap_consumer)),
    )
}

/// Run the callback in real time (one block every `BLOCK / SR` seconds) until
/// `done` flips, then return the block count.
fn drive(engine: &mut Engine, done: &AtomicBool) -> u64 {
    let period = Duration::from_secs_f64(BLOCK as f64 / SR);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !done.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
        run_block(&mut engine.state, &mut engine.output);
        std::thread::sleep(period);
    }
    engine.stats.blocks_processed()
}

#[test]
fn a_capture_of_the_live_graph_measures_the_test_tone() {
    let (mut engine, service) = engine(true);
    assert_eq!(
        service.sample_rate(),
        SR,
        "building the RT state publishes the device rate to the tap"
    );

    let done = Arc::new(AtomicBool::new(false));
    let capture_service = Arc::clone(&service);
    let capture_done = Arc::clone(&done);
    let capturing = std::thread::spawn(move || {
        let result = capture_service.capture(Duration::from_millis(500), Duration::from_secs(10));
        capture_done.store(true, Ordering::Relaxed);
        result
    });

    let blocks = drive(&mut engine, &done);
    let capture = capturing.join().expect("the capture thread").expect(
        "the engine is playing a tone, so half a second of audio must arrive inside ten seconds",
    );

    assert!(blocks > 0, "the callback should have run");
    assert_eq!(capture.frames(), 24_000, "0.5 s at 48 kHz");
    assert_eq!(capture.channels, 2);
    assert_eq!(capture.sample_rate, SR);
    assert_eq!(
        capture.dropped, 0,
        "the ring is sized to never overflow here"
    );

    let stats = capture.stats();
    assert_eq!(stats.per_channel.len(), 2);
    for channel in &stats.per_channel {
        assert!(
            (channel.peak - DEFAULT_AMPLITUDE).abs() < 0.005,
            "{} peaked at {} (test-tone is {DEFAULT_AMPLITUDE})",
            channel.label,
            channel.peak
        );
        // -14 dBFS, the test tone's documented level.
        assert!(
            (channel.peak_dbfs - (-13.98)).abs() < 0.1,
            "{} peaked at {} dBFS",
            channel.label,
            channel.peak_dbfs
        );
        let dominant = channel.dominant_hz.expect("a tone has a dominant bin");
        assert!(
            (dominant - DEFAULT_FREQUENCY_HZ).abs() < 1.0,
            "{} reported {dominant} Hz, expected {DEFAULT_FREQUENCY_HZ}",
            channel.label
        );
        assert_eq!(channel.clipped, 0);
        assert_eq!(channel.non_finite, 0);
        assert!(channel.dc_offset.abs() < 1e-3);
        assert!(
            channel.spectral_flatness < 0.05,
            "a pure tone is not noise: {}",
            channel.spectral_flatness
        );
    }

    // The two channels of the test tone are identical, sample for sample.
    assert_eq!(capture.channel(0), capture.channel(1));

    let summary = capture.summary_text();
    assert!(summary.contains("24000 frames"), "{summary}");
    assert!(summary.contains("440"), "{summary}");
}

#[test]
fn a_capture_while_the_transport_is_stopped_records_the_gates_silence() {
    // The gate silences the bus, but the callback still runs, so the tap fills
    // up with zeroes: a *successful* capture of digital silence, which is a very
    // different diagnosis from "nothing arrived".
    let (mut engine, service) = engine(false);

    let done = Arc::new(AtomicBool::new(false));
    let capture_service = Arc::clone(&service);
    let capture_done = Arc::clone(&done);
    let capturing = std::thread::spawn(move || {
        let result = capture_service.capture(Duration::from_millis(100), Duration::from_secs(10));
        capture_done.store(true, Ordering::Relaxed);
        result
    });

    drive(&mut engine, &done);
    let capture = capturing
        .join()
        .expect("the capture thread")
        .expect("a stopped transport still produces blocks, so the capture fills with silence");

    assert_eq!(capture.frames(), 4_800);
    assert!(capture.samples.iter().all(|s| *s == 0.0));

    let stats = capture.stats();
    assert_eq!(stats.per_channel[0].peak, 0.0);
    assert_eq!(stats.per_channel[0].peak_dbfs, f32::NEG_INFINITY);
    assert!(stats.per_channel[0].dominant_hz.is_none());

    // Serde-safety of -inf, on a capture that came out of the real RT path.
    let json = serde_json::to_value(&stats).expect("stats must serialise");
    assert!(json["perChannel"][0]["peakDbfs"].is_null());
}

#[test]
fn a_capture_with_no_callback_running_times_out_and_says_how_far_it_got() {
    // Nothing drives `run_block`, which is what a wedged or never-started audio
    // thread looks like from the outside.
    let (engine, service) = engine(true);

    let error = service
        .capture(Duration::from_millis(200), Duration::from_millis(150))
        .expect_err("no blocks are being processed");

    match error {
        CaptureError::TimedOutWithSamples { got, want, capture } => {
            assert_eq!(got, 0);
            assert_eq!(want, 9_600);
            assert!(capture.is_empty());
            assert_eq!(capture.sample_rate, SR);
        }
        other => panic!("expected a timeout carrying partial audio, got {other}"),
    }
    assert!(
        !service.is_armed(),
        "the tap must be disarmed after a timeout"
    );
    drop(engine);
}

#[test]
fn a_capture_survives_the_rt_state_being_torn_down_and_rebuilt() {
    // The stop/start cycle: `RtStateCarrier` hands the tap back, and a new
    // `RtState` re-publishes the sample rate. Here that is simulated directly.
    let (mut engine, service) = engine(true);
    let tap = engine
        .state
        .take_debug_tap()
        .expect("the first state owns the tap");
    assert_eq!(service.sample_rate(), SR);

    let gc = GcThread::spawn();
    let (_graph, slot) = graph_channel(4, gc.handle());
    let (_commands_tx, commands) = rt_channel::<RtCommand>(16);
    let (_notes_tx, notes) = note_channel(NOTE_CAPACITY);
    let (_song_tx, song) = song_channel(SONG_QUEUE_CAPACITY, gc.handle());
    let mut rebuilt = RtState::new(RtStateParams {
        graph: slot,
        commands,
        notes,
        note_capacity: NOTE_CAPACITY,
        song,
        meters: None,
        debug_tap: Some(tap),
        spectrum: None,
        transport: Arc::new(TransportClock::new(44_100.0)),
        sample_rate: 44_100.0,
        out_channels: 2,
        max_block: MAX_BLOCK,
        stats: Arc::new(AudioIoStats::default()),
    });

    assert_eq!(
        service.sample_rate(),
        44_100.0,
        "a restart on a different device must update the reported rate"
    );

    // And the rebuilt state still feeds the tap.
    service.shared().arm();
    let mut output = vec![0.0_f32; 64 * 2];
    run_block(&mut rebuilt, &mut output);
    service.shared().disarm();
    assert!(service.dropped() == 0);
}
