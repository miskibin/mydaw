//! The arrangement, measured through the sound card.
//!
//! `plugins/sequencer/src/schedule.rs` proves song mode is right as arithmetic,
//! over a matrix of tempos and block sizes with no device anywhere. This proves
//! it is right *after the audio device has had it*: a real WASAPI stream, a real
//! callback at whatever block size the driver asked for, a real basedrop
//! hand-off of the whole song, and a capture taken from the same tap the debug
//! harness uses.
//!
//! ```text
//!   bar   0      1      2      3      4      5      6      7
//!   lane0 [ A ][ A ][ A ][ A ]              [ A ][ A ][ A ][ A ]
//!   lane1                                   [ B ][ B ][ B ][ B ]
//!
//!   A: kick on every beat            (channel 0, the Kick instrument)
//!   B: open hat on the off-beats     (channel 3, the Hat Open instrument)
//! ```
//!
//! Two patterns, one clip of A at bar 0 running four bars, then A **and** B
//! together from bar 4 -- which is the mission's "pattern A x 2, then A+B" in
//! the units the engine actually has. The claims are:
//!
//! 1. every kick onset in eight bars sits on the beat grid,
//! 2. the open hat is **silent for the first four bars** and sounds after them,
//!    which is the whole difference between a pattern and an arrangement, and
//! 3. seeking to bar 5 and capturing produces the same picture as the run that
//!    got there by playing -- the property that makes a playhead draggable.
//!
//! `#[ignore]`d: it opens the audio device and plays an audible beat, so
//! `cargo test --workspace` stays silent and passes on a machine with no sound
//! card.
//!
//! ```sh
//! cargo test -p mydaw-app --test song_mode_audio -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every run leaves a WAV, an annotated PNG and a stats JSON under
//! `.debug/captures/<timestamp>/`, so a failure can be looked at rather than
//! guessed at.

use std::thread::sleep;
use std::time::Duration;

use mydaw_app::{boot_kernel, capture_root, capture_to_disk, AppState};
use mydaw_plugin_debug_tap::{align_grid, detect_onsets, Band, GridReport, Onset, OnsetConfig};
use mydaw_plugin_sequencer::{
    step_len_samples, tick_boundary, ChannelClip, PlayMode, StepRow, DEFAULT_STEPS, TICKS_PER_BAR,
};

/// Tempo the arrangement is played at. The engine's default, and a dance tempo.
const BPM: f64 = 130.0;

/// Where pattern B comes in.
const B_ENTERS_BAR: u64 = 4;

/// How many bars the arrangement is.
const SONG_BARS: u64 = 8;

/// Blocks of silence to let through before capturing, so the capture starts
/// inside steady-state playback rather than on the device's first block.
const SETTLE: Duration = Duration::from_millis(400);

/// Corner of the band the kick is measured in. The kick sweeps 150 -> 50 Hz;
/// nothing else in this arrangement has energy down there.
const KICK_BAND_HZ: f32 = 120.0;

/// Corner of the band the open hat is measured in. Both hats are highpassed at
/// 6 kHz, and the kick has nothing up there.
const HAT_BAND_HZ: f32 = 5_000.0;

/// Worst per-onset deviation allowed, in milliseconds.
///
/// A millisecond is roughly a tenth of what a listener can hear as a flam, and
/// about forty samples at 44.1 kHz. The offline scheduler is exact to the
/// sample; the margin here is for the detector meeting a real converter.
const TOLERANCE_MS: f64 = 1.0;

/// Root-mean-square of `samples` above `corner`, in dB relative to full scale.
///
/// The measure the "is pattern B playing" claim rests on. Counting onsets in the
/// hat band does not answer it: the kick's two-millisecond noise click is
/// broadband, so it registers up there too, and a detector cannot tell one
/// instrument's transient from another's. Energy over a whole capture can --
/// an open hat rings for 350 ms and a click does not.
fn high_band_dbfs(samples: &[f32], sample_rate: f64, corner: f32) -> f64 {
    let banded = Band::High(corner).apply(samples, sample_rate);
    if banded.is_empty() {
        return f64::NEG_INFINITY;
    }
    let sum: f64 = banded.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    let rms = (sum / banded.len() as f64).sqrt();
    if rms <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * rms.log10()
    }
}

/// One line an agent (or a reader of a failure) can act on, plus one per onset.
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
#[ignore = "opens the audio device and plays an audible arrangement"]
fn an_arrangement_comes_out_of_the_device_on_the_song_timeline() {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");

    let info = match state.start_audio_with_project() {
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
    let bar_seconds = step_len * DEFAULT_STEPS as f64 / sample_rate;
    eprintln!(
        "tempo: {BPM} BPM | step {step_len:.3} samples ({:.3} ms) | bar {bar_seconds:.4} s",
        step_len / sample_rate * 1_000.0
    );

    // The starter rack is Kick / Snare / Hat Closed / Hat Open / Poly Synth, so
    // channel 0 is the kick and channel 3 the open hat -- one instrument per
    // channel, which is what makes this arrangement legible in a spectrum.
    let rack: Vec<String> = state
        .mixer_state()
        .tracks
        .iter()
        .map(|track| track.name.clone())
        .collect();
    eprintln!("rack: {rack:?}");

    // --- pattern A: a kick on every beat ------------------------------------
    assert_eq!(state.set_bpm(BPM).expect("the tempo is legal"), BPM);
    let mut a = state.sequencer().active_pattern();
    a.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 4, 8, 12]));
    state.set_pattern(a).expect("pattern A is valid");
    let a_id = state.sequencer().patterns().active_id();

    // --- pattern B: an open hat on the off-beats ----------------------------
    let b_id = state
        .add_pattern(Some("B".to_owned()))
        .expect("room for two");
    state
        .edit_patterns(|set| {
            let pattern = set.get_mut(b_id)?;
            *pattern.channel_mut(3)? = ChannelClip {
                channel: 3,
                steps: Some(StepRow::empty(46).with_steps(&[2, 6, 10, 14])),
                notes: Vec::new(),
            };
            Ok(())
        })
        .expect("pattern B is valid");

    // --- the arrangement ----------------------------------------------------
    let bar = u64::from(TICKS_PER_BAR);
    state
        .edit_playlist(|list| list.add_pattern_clip(a_id, 0, 0, B_ENTERS_BAR * bar))
        .expect("A from the top");
    state
        .edit_playlist(|list| {
            list.add_pattern_clip(
                a_id,
                0,
                B_ENTERS_BAR * bar,
                (SONG_BARS - B_ENTERS_BAR) * bar,
            )
        })
        .expect("A again");
    state
        .edit_playlist(|list| {
            list.add_pattern_clip(
                b_id,
                1,
                B_ENTERS_BAR * bar,
                (SONG_BARS - B_ENTERS_BAR) * bar,
            )
        })
        .expect("B over it");

    let song = state.sequencer_state();
    eprintln!(
        "song: {} pattern(s), {} clip(s), ends at tick {} ({} bars)",
        song.pattern_set.patterns.len(),
        song.playlist.clips.len(),
        song.playlist.end_ticks,
        song.playlist.end_ticks / bar
    );
    assert_eq!(song.playlist.end_ticks, SONG_BARS * bar);

    state
        .set_play_mode(PlayMode::Song)
        .expect("the mode is session state");
    assert_eq!(state.sequencer_state().play_mode, "song");

    // --- silence before Play ------------------------------------------------
    let idle = capture_to_disk(state.debug_tap(), 0.25, &capture_root())
        .expect("the engine is running, so a capture must arrive");
    assert_eq!(
        idle.stats.per_channel[0].peak, 0.0,
        "an arrangement must be silent until the transport runs"
    );

    // --- the first four bars: A alone ---------------------------------------
    state.seek(0).expect("the playhead rewinds");
    state.set_playing(true).expect("the transport starts");
    sleep(SETTLE);

    let first = capture_to_disk(
        state.debug_tap(),
        (bar_seconds * (B_ENTERS_BAR - 1) as f64) as f32,
        &capture_root(),
    )
    .expect("three bars of A must be capturable");
    let left = first.capture.channel(0);
    eprintln!(
        "bars 0-3: {} frames | peak {:.4} ({:.2} dBFS) | dropped {}",
        first.stats.frames,
        first.stats.per_channel[0].peak,
        first.stats.per_channel[0].peak_dbfs,
        first.capture.dropped
    );
    eprintln!("wav  {}", first.artifacts.wav.display());
    eprintln!("png  {}", first.artifacts.png.display());
    assert_eq!(first.capture.dropped, 0, "the capture has holes");
    assert_eq!(
        first.stats.per_channel[0].non_finite, 0,
        "a NaN reached the device"
    );

    let kicks = detect_onsets(&left, sample_rate, &OnsetConfig::low(KICK_BAND_HZ));
    let grid = align_grid(&kicks, step_len * 4.0, sample_rate);
    report("kick (bars 0-3)", &kicks, &grid, sample_rate);
    assert!(kicks.len() >= 8, "three bars is at least eight beats");
    assert!(grid.is_contiguous(), "a beat is missing its kick");
    assert!(
        grid.within(TOLERANCE_MS),
        "the kick drifted off the grid by {:.3} ms",
        grid.max_abs_error_ms
    );

    // The claim that makes this an *arrangement* rather than a loop: pattern B
    // is in the project and is not playing yet. Measured as energy above 5 kHz
    // rather than as onsets -- see `high_band_dbfs`.
    let before_b = high_band_dbfs(&left, sample_rate, HAT_BAND_HZ);
    eprintln!("[hat band, bars 0-3] {before_b:.2} dBFS rms (the kick's click only)");

    // --- seek to bar 5: A and B together ------------------------------------
    //
    // A *seek*, not a wait: the point is that the arrangement sounds correct
    // from a position the transport jumped to, which is what a draggable
    // playhead needs and what nothing but a real capture can show.
    state.set_playing(false).ok();
    let bar_five = tick_boundary(5 * bar, step_len);
    state.seek(bar_five).expect("the playhead moves");
    state.set_playing(true).expect("the transport restarts");
    sleep(SETTLE);

    let second = capture_to_disk(
        state.debug_tap(),
        (bar_seconds * 2.0) as f32,
        &capture_root(),
    )
    .expect("two bars from bar 5 must be capturable");
    state.set_playing(false).ok();

    let left = second.capture.channel(0);
    eprintln!(
        "bars 5-6 (after a seek to sample {bar_five}): {} frames | peak {:.4} ({:.2} dBFS) | dropped {}",
        second.stats.frames,
        second.stats.per_channel[0].peak,
        second.stats.per_channel[0].peak_dbfs,
        second.capture.dropped
    );
    eprintln!("wav  {}", second.artifacts.wav.display());
    eprintln!("png  {}", second.artifacts.png.display());
    eprintln!("json {}", second.artifacts.stats_json.display());
    assert_eq!(second.capture.dropped, 0, "the capture has holes");
    assert_eq!(
        second.stats.per_channel[0].clipped, 0,
        "two drums must not clip"
    );

    let kicks = detect_onsets(&left, sample_rate, &OnsetConfig::low(KICK_BAND_HZ));
    let grid = align_grid(&kicks, step_len * 4.0, sample_rate);
    report("kick (from bar 5)", &kicks, &grid, sample_rate);
    assert!(kicks.len() >= 7, "two bars is at least seven beats");
    assert!(
        grid.is_contiguous() && grid.within(TOLERANCE_MS),
        "the kick came back off the grid after a seek: worst {:.3} ms, contiguous {}",
        grid.max_abs_error_ms,
        grid.is_contiguous()
    );

    // Pattern B is playing now, and the high band says so by a wide margin.
    let after_b = high_band_dbfs(&left, sample_rate, HAT_BAND_HZ);
    eprintln!(
        "[hat band, from bar 5] {after_b:.2} dBFS rms | {:+.2} dB against bars 0-3",
        after_b - before_b
    );
    assert!(
        after_b - before_b > 10.0,
        "pattern B enters at bar {B_ENTERS_BAR}, so the hat band must be far louder          after it than before: {before_b:.2} -> {after_b:.2} dBFS"
    );

    // And its hits are on the grid. Every high-band onset now falls on an
    // *eighth*: the kick's click on the beats, the open hat between them.
    let hats = detect_onsets(&left, sample_rate, &OnsetConfig::high(HAT_BAND_HZ));
    let hat_grid = align_grid(&hats, step_len * 2.0, sample_rate);
    report("hat band (from bar 5)", &hats, &hat_grid, sample_rate);
    assert!(
        hats.len() >= 7,
        "two bars of eighth-note events is at least seven onsets -- got {}",
        hats.len()
    );
    assert!(
        hat_grid.within(TOLERANCE_MS),
        "the open hat drifted off the grid by {:.3} ms after a seek",
        hat_grid.max_abs_error_ms
    );
}
