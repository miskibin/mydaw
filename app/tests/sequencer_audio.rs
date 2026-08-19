//! The step sequencer, measured through the sound card.
//!
//! `plugins/audio-io/tests/sequencer_engine.rs` proves the beat is right in an
//! offline render. This proves it is right *after the audio device has had it*:
//! a real WASAPI stream, a real callback at whatever block size the driver asked
//! for, a real basedrop hand-off of the pattern, and a capture taken from the
//! same tap the debug harness uses.
//!
//! What is asserted is timing, not level. The programmed beat is four-on-the
//! -floor kick, snare on 2 and 4, eighth-note hi-hats at 130 BPM, and the claim
//! is that every onset in the captured audio sits on the tempo grid to within a
//! fraction of a millisecond. That is the thing a sequencer is *for*, and it is
//! the thing an ear cannot verify to that precision.
//!
//! `#[ignore]`d: it opens the audio device and plays an audible beat, so
//! `cargo test --workspace` stays silent and passes on a machine with no sound
//! card.
//!
//! ```sh
//! cargo test -p mydaw-app --test sequencer_audio -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every run leaves a WAV, an annotated PNG and a stats JSON under
//! `.debug/captures/<timestamp>/`, plus a `grid.json` next to them carrying the
//! per-onset deviations -- so a failure can be looked at rather than guessed at.

use std::thread::sleep;
use std::time::Duration;

use mydaw_app::{boot_kernel, capture_root, capture_to_disk, AppState, Source};
use mydaw_plugin_debug_tap::{align_grid, detect_onsets, GridReport, Onset, OnsetConfig};
use mydaw_plugin_sequencer::{step_len_samples, Pattern, PatternRow, Step, STEPS};

/// Tempo the beat is programmed at. The engine's default, and a dance tempo.
const BPM: f64 = 130.0;

/// Bars to capture. Two is enough to see the loop wrap and still be short.
const BARS: usize = 2;

/// Blocks of silence to let through before capturing, so the capture starts
/// inside steady-state playback rather than on the device's first block.
const SETTLE: Duration = Duration::from_millis(400);

/// Corner of the band the kick is measured in. The kit's kick sweeps 150 -> 50
/// Hz; nothing else in the kit has energy down there.
const KICK_BAND_HZ: f32 = 120.0;

/// Corner of the band the hats are measured in.
const HAT_BAND_HZ: f32 = 4_000.0;

/// Worst per-onset deviation allowed, in milliseconds.
///
/// A millisecond is roughly a tenth of what a listener can hear as a flam, and
/// about forty samples at 44.1 kHz. The offline render manages 0.05 ms; the
/// margin here is for the detector meeting a real converter, not for the engine.
const TOLERANCE_MS: f64 = 1.0;

/// The demo beat: fourteen clicks in the grid -- four kicks, two snares and
/// eight hats.
fn demo_beat() -> Pattern {
    let mut pattern = Pattern::default();
    // Rows are kick / snare / closed hat / open hat, in that order.
    pattern.rows[0] = PatternRow::empty(pattern.rows[0].key).with_steps(&[0, 4, 8, 12]);
    pattern.rows[1] = PatternRow::empty(pattern.rows[1].key).with_steps(&[4, 12]);
    let mut hats = PatternRow::empty(pattern.rows[2].key);
    for step in (0..STEPS).step_by(2) {
        hats.steps[step] = Step::on();
    }
    pattern.rows[2] = hats;
    pattern
}

/// Print a grid report as one line an agent can read, and one line per onset.
fn report(label: &str, onsets: &[Onset], grid: &GridReport, sample_rate: f64) {
    eprintln!(
        "[{label}] {} onsets | worst {:.3} ms ({:.1} samples) | mean {:.3} ms | contiguous {}",
        onsets.len(),
        grid.max_abs_error_ms,
        grid.max_abs_error_samples,
        grid.mean_abs_error_ms,
        grid.is_contiguous(),
    );
    for deviation in &grid.deviations {
        eprintln!(
            "[{label}]   line {:>3}  frame {:>8}  t {:>8.4} s  err {:+.3} ms",
            deviation.line,
            deviation.frame,
            deviation.frame as f64 / sample_rate,
            deviation.error_ms,
        );
    }
}

#[test]
#[ignore = "opens the audio device and plays an audible beat"]
fn a_programmed_beat_comes_out_of_the_device_on_the_tempo_grid() {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");

    // The drum kit is the instrument under the grid. Started before the pattern
    // is programmed, exactly as a session would: choose an instrument, then
    // click a beat into it.
    let info = match state.start_audio_with_source(Source::DrumKit, None, None) {
        Ok(info) => info,
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            return;
        }
    };
    eprintln!(
        "device: {} @ {} Hz, {} ch (max block {})",
        info.device_name, info.sample_rate, info.channels, info.max_block
    );

    let sample_rate = f64::from(info.sample_rate);
    let step_len = step_len_samples(sample_rate, BPM);
    let bar_seconds = step_len * STEPS as f64 / sample_rate;
    eprintln!(
        "tempo: {BPM} BPM | step {step_len:.3} samples ({:.3} ms) | bar {bar_seconds:.4} s",
        step_len / sample_rate * 1_000.0
    );

    // --- programme the beat, exactly as the panel does ----------------------
    assert_eq!(state.set_bpm(BPM).expect("the tempo is legal"), BPM);
    state.set_pattern(demo_beat()).expect("the beat is valid");
    assert_eq!(
        state.sequencer().pattern().hit_count(),
        14,
        "four kicks, two snares, eight hats"
    );

    // --- silence before Play ------------------------------------------------
    let idle = capture_to_disk(state.debug_tap(), 0.25, &capture_root())
        .expect("the engine is running, so a capture must arrive");
    assert_eq!(
        idle.stats.per_channel[0].peak, 0.0,
        "a programmed pattern must be silent until the transport runs"
    );

    // --- play, settle, capture two bars -------------------------------------
    state.set_playing(true).expect("the transport starts");
    sleep(SETTLE);

    let seconds = (bar_seconds * BARS as f64) as f32;
    let outcome = capture_to_disk(state.debug_tap(), seconds, &capture_root())
        .expect("two bars of a playing beat must be capturable");
    state.set_playing(false).ok();

    let stats = &outcome.stats;
    let channel = &stats.per_channel[0];
    eprintln!(
        "capture: {} frames @ {} Hz ({:.3} s) | peak {:.4} ({:.2} dBFS) | rms {:.4} | dropped {}",
        stats.frames,
        stats.sample_rate,
        stats.duration_seconds,
        channel.peak,
        channel.peak_dbfs,
        channel.rms,
        outcome.capture.dropped,
    );
    eprintln!("wav  {}", outcome.artifacts.wav.display());
    eprintln!("png  {}", outcome.artifacts.png.display());
    eprintln!("json {}", outcome.artifacts.stats_json.display());

    assert_eq!(
        outcome.capture.dropped, 0,
        "the capture has holes; its timings cannot be trusted"
    );
    assert_eq!(channel.non_finite, 0, "a NaN reached the device");
    assert_eq!(channel.clipped, 0, "a three-piece beat must not clip");
    assert!(channel.peak > 0.1, "the beat must be audible");

    let left = outcome.capture.channel(0);

    // --- the kick: eight hits, on the beat ----------------------------------
    let kicks = detect_onsets(&left, sample_rate, &OnsetConfig::low(KICK_BAND_HZ));
    let kick_grid = align_grid(&kicks, step_len * 4.0, sample_rate);
    report("kick", &kicks, &kick_grid, sample_rate);
    assert_eq!(
        kicks.len(),
        BARS * 4,
        "two bars of four-on-the-floor is eight kicks"
    );
    assert!(kick_grid.is_contiguous(), "a beat is missing its kick");
    assert!(
        kick_grid.within(TOLERANCE_MS),
        "the kick drifted off the grid by {:.3} ms",
        kick_grid.max_abs_error_ms
    );

    // --- the hats: sixteen hits, on the eighths -----------------------------
    let hats = detect_onsets(&left, sample_rate, &OnsetConfig::high(HAT_BAND_HZ));
    let hat_grid = align_grid(&hats, step_len * 2.0, sample_rate);
    report("hat", &hats, &hat_grid, sample_rate);
    assert_eq!(hats.len(), BARS * 8, "two bars of eighths is sixteen hits");
    assert!(hat_grid.is_contiguous(), "an eighth is missing");

    // The off-beat hats sound alone; steps 4 and 12 carry a snare as well, and
    // the detector's latency depends on the attack it is looking at. The tight
    // assertion therefore goes where the source is single -- see the module
    // docs of `mydaw_plugin_debug_tap::onset`.
    let offbeat: Vec<Onset> = hats
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, onset)| *onset)
        .collect();
    let offbeat_grid = align_grid(&offbeat, step_len * 4.0, sample_rate);
    report("off-beat hat", &offbeat, &offbeat_grid, sample_rate);
    assert!(
        offbeat_grid.within(TOLERANCE_MS),
        "the off-beat hats drifted off the grid by {:.3} ms",
        offbeat_grid.max_abs_error_ms
    );

    // --- the kick really is a kick ------------------------------------------
    // Timing is the claim, but a "kick" band with no kick in it would satisfy
    // every assertion above by measuring the wrong thing. The capture's loudest
    // partial has to be down where a kick lives.
    let dominant = channel
        .dominant_hz
        .expect("a beat with a kick in it has a spectral peak");
    eprintln!(
        "spectrum: dominant {dominant:.2} Hz | flatness {:.4} | top {:?}",
        channel.spectral_flatness,
        channel
            .top_bins
            .iter()
            .map(|peak| (peak.frequency_hz.round(), peak.magnitude_dbfs.round()))
            .collect::<Vec<_>>()
    );
    assert!(
        dominant < 100.0,
        "the loudest partial of a four-on-the-floor should be the kick, measured {dominant:.1} Hz"
    );

    // --- leave the numbers on disk ------------------------------------------
    let grid_json = outcome.artifacts.dir.join("grid.json");
    let payload = serde_json::json!({
        "bpm": BPM,
        "sampleRate": sample_rate,
        "stepSamples": step_len,
        "bars": BARS,
        "toleranceMs": TOLERANCE_MS,
        "kick": kick_grid,
        "hat": hat_grid,
        "offbeatHat": offbeat_grid,
    });
    std::fs::write(&grid_json, serde_json::to_vec_pretty(&payload).unwrap())
        .expect("the capture directory exists");
    eprintln!("grid {}", grid_json.display());

    // --- Stop must not leave a note hanging ---------------------------------
    sleep(Duration::from_millis(600));
    let after_stop = capture_to_disk(state.debug_tap(), 0.25, &capture_root())
        .expect("the engine is still running");
    assert_eq!(
        after_stop.stats.per_channel[0].peak, 0.0,
        "the grid must fall silent when the transport stops"
    );

    state.meters().shutdown();
    state.audio().stop();
}
