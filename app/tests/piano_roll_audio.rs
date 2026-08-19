//! The piano roll, measured through the sound card.
//!
//! `plugins/sequencer/src/schedule.rs` proves the tick arithmetic offline;
//! `app/tests/sequencer_audio.rs` proves the drum grid survives a real device.
//! This proves the half step 9 added: that a **melody** written in ticks comes
//! out of the speakers at the right pitches, at the right times, *while the drum
//! grid is playing underneath it* -- one pattern, one scheduler, two tracks.
//!
//! ```text
//!   track 0  Drum Kit    kick on every quarter, hats on every eighth
//!   track 1  Poly Synth  C4  E4  G4  C5   C4  E4  G4  C5     (quarter notes)
//!            |-------- bar 1 --------|-------- bar 2 --------|
//! ```
//!
//! What is asserted is **timing and pitch**, not level:
//!
//! 1. Every kick onset sits on the quarter-note grid to within a millisecond,
//!    using the same detector the step-4 verification uses.
//! 2. Those onsets then *define* the grid -- the capture starts at an unknowable
//!    phase of the loop, because arming the tap and starting the transport are
//!    different events -- and each quarter-note window between them is
//!    transformed and its dominant partial compared with the note the pattern
//!    says is sounding there.
//!
//! The rotation of the arpeggio within the capture is recovered rather than
//! assumed: the figure repeats every four quarters, so there are four candidate
//! alignments, and the one with the smallest total pitch error is chosen *before*
//! anything is asserted. A pattern playing the wrong notes fits none of them.
//!
//! `#[ignore]`d: it opens the audio device and plays an audible arpeggio, so
//! `cargo test --workspace` stays silent and passes on a machine with no sound
//! card.
//!
//! ```sh
//! cargo test -p mydaw-app --test piano_roll_audio -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every run leaves a WAV, an annotated PNG and a stats JSON under
//! `.debug/captures/<timestamp>/`, plus a `pitch.json` carrying the per-window
//! measurements -- so a failure can be looked at rather than guessed at.

use std::thread::sleep;
use std::time::Duration;

use mydaw_app::{
    boot_kernel, capture_root, capture_to_disk, AppState, ChannelClipDto, MelodicNoteDto,
    PatternDto, StepDto, StepRowDto,
};
use mydaw_plugin_debug_tap::{
    align_grid, detect_onsets, spectrum, Band, GridReport, Onset, OnsetConfig,
};
use mydaw_plugin_sequencer::{step_len_samples, DEFAULT_STEP_VELOCITY, TICKS_PER_STEP};

/// Tempo. The engine's default, and the one the step-4 verification uses.
const BPM: f64 = 130.0;

/// Bars in the pattern. Two, so the loop wrap is inside the capture.
const BARS: usize = 2;

/// Steps in the pattern: two bars of sixteenths.
const STEPS: usize = BARS * 16;

/// Ticks in a quarter note. The arpeggio's note length.
const QUARTER_TICKS: u32 = TICKS_PER_STEP * 4;

/// Note channel the melody plays on. Track 1 of the default project is the poly
/// synth; a track's index *is* its note channel.
const SYNTH_CHANNEL: u8 = 1;

/// The arpeggio, in MIDI keys: C4, E4, G4, C5.
const ARPEGGIO: [u8; 4] = [60, 64, 67, 72];

/// Blocks of playback to let through before capturing, so the capture starts
/// inside steady-state playback rather than on the device's first block.
const SETTLE: Duration = Duration::from_millis(500);

/// Corner of the band the kick is measured in. The kit's kick sweeps 150 -> 50
/// Hz; nothing else in the kit has energy down there.
const KICK_BAND_HZ: f32 = 120.0;

/// Worst per-onset deviation allowed, in milliseconds. As step 4.
const TOLERANCE_MS: f64 = 1.0;

/// Band the pitch is measured in, in Hz.
///
/// The high corner removes the kick's fundamental *and* the synth's own
/// sub-oscillator, which is a sine an octave below the note at -6 dB and would
/// otherwise be a plausible-looking wrong answer. The low corner removes the
/// hats, which are broadband noise several kHz up. What is left in between is
/// the fundamental of whichever note is sounding, and its lower harmonics.
const PITCH_BAND_LOW_HZ: f32 = 200.0;
const PITCH_BAND_HIGH_HZ: f32 = 1_500.0;

/// The part of each quarter-note window that is analysed, as fractions of it.
///
/// Not the whole window: the first fifth still contains the previous note's
/// 120 ms release tail and this note's attack, and the last fifth is near enough
/// the next kick to catch its transient. The middle is where exactly one note is
/// sounding at its sustain level, which is the only part that can answer "what
/// pitch is this?" without qualification.
const WINDOW_FROM: f64 = 0.40;
const WINDOW_TO: f64 = 0.80;

/// Equal-tempered frequency of a MIDI key, A4 = 440 Hz.
fn key_hz(key: u8) -> f64 {
    440.0 * 2.0_f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// The two-bar pattern, in the shape the webview sends.
///
/// Built as a [`PatternDto`] and pushed through `set_pattern_dto` rather than
/// assembled as a `Pattern` directly: that is the path a click in the piano roll
/// takes, validation included, and a hardware test that skipped it would be
/// verifying a pattern no user can produce.
fn arpeggio_over_a_beat() -> PatternDto {
    let step = |on: bool| StepDto {
        on,
        velocity: DEFAULT_STEP_VELOCITY,
    };
    let drum = |channel: u8, key: u8, hit: &dyn Fn(usize) -> bool| ChannelClipDto {
        channel,
        mode: String::new(),
        steps: Some(StepRowDto {
            key,
            label: String::new(),
            steps: (0..STEPS).map(|index| step(hit(index))).collect(),
        }),
        notes: Vec::new(),
    };

    PatternDto {
        id: 1,
        name: "Pattern 1".to_owned(),
        color: None,
        steps: STEPS as u8,
        channels: vec![
            // Kick on every quarter -- four on the floor, and the grid every
            // melodic window is measured against.
            drum(0, 36, &|index| index % 4 == 0),
            // Closed hats on every eighth.
            drum(2, 42, &|index| index % 2 == 0),
            // The melody, on its own channel: one clip per channel, and a
            // channel with notes is a piano roll rather than a grid.
            ChannelClipDto {
                channel: SYNTH_CHANNEL,
                mode: String::new(),
                steps: None,
                notes: (0..BARS * 4)
                    .map(|quarter| MelodicNoteDto {
                        key: ARPEGGIO[quarter % ARPEGGIO.len()],
                        start: quarter as u32 * QUARTER_TICKS,
                        length: QUARTER_TICKS,
                        velocity: DEFAULT_STEP_VELOCITY,
                    })
                    .collect(),
            },
        ],
    }
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

/// Drop leading onsets that are not a quarter note from the one after them.
///
/// The onset detector's band filter starts from zero state, so a capture that
/// begins in the *middle* of a sounding note opens with an artificial rise in
/// its first milliseconds -- a feature of the measurement, not of the audio.
/// This capture always begins mid-loop, so that rise is always there.
///
/// It is removed by the one thing that distinguishes it from a drum: it is not
/// a quarter note away from its neighbour. Nothing else is trimmed, and if the
/// engine's spacing were wrong this would trim everything and the contiguity
/// assertion below would fail loudly rather than quietly passing on two onsets.
fn trim_warm_up(onsets: &[Onset], quarter: f64) -> Vec<Onset> {
    let mut trimmed = onsets;
    while trimmed.len() >= 2 {
        let gap = (trimmed[1].frame - trimmed[0].frame) as f64;
        // Five per cent of a quarter is 23 ms: twenty times the jitter a real
        // onset shows, and far less than the distance any spurious one is at.
        if (gap - quarter).abs() <= quarter * 0.05 {
            break;
        }
        trimmed = &trimmed[1..];
    }
    trimmed.to_vec()
}

/// The dominant partial of `samples`, in Hz, or `None` when there is no energy.
fn dominant_hz(samples: &[f32], sample_rate: f64) -> Option<(f64, f64)> {
    let high = Band::High(PITCH_BAND_LOW_HZ).apply(samples, sample_rate);
    let banded = Band::Low(PITCH_BAND_HIGH_HZ).apply(&high, sample_rate);
    let spectrum = spectrum(&banded, sample_rate);
    let peak = spectrum.dominant()?;
    Some((peak.frequency_hz, spectrum.bin_hz))
}

#[test]
#[ignore = "opens the audio device and plays an audible arpeggio over a beat"]
fn a_pattern_of_notes_and_drums_comes_out_of_the_device_in_tune_and_on_the_grid() {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");

    // The default project is exactly the rig this test needs: track 0 is the
    // drum kit and track 1 the poly synth, so channel 0 is the grid's and
    // channel 1 is the roll's.
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
    let quarter_len = step_len * 4.0;
    let bar_seconds = step_len * 16.0 / sample_rate;
    eprintln!(
        "tempo: {BPM} BPM | step {step_len:.3} samples | quarter {quarter_len:.3} samples \
         ({:.1} ms) | bar {bar_seconds:.4} s",
        quarter_len / sample_rate * 1_000.0
    );

    // --- programme the pattern, exactly as the panels do --------------------
    assert_eq!(state.set_bpm(BPM).expect("the tempo is legal"), BPM);
    state
        .set_pattern_dto(&arpeggio_over_a_beat())
        .expect("the pattern is valid");
    let stored = state.sequencer().active_pattern();
    assert_eq!(stored.length_steps as usize, STEPS, "two bars");
    assert_eq!(stored.hit_count(), 8 + 16, "eight kicks, sixteen hats");
    assert_eq!(stored.note_count(), 8, "two times round a four-note figure");

    // --- play, settle, capture ----------------------------------------------
    state.set_playing(true).expect("the transport starts");
    sleep(SETTLE);

    // A quarter more than two bars, so that however the capture lands inside the
    // loop there are eight whole quarters with a kick at the start of each.
    let seconds = (bar_seconds * BARS as f64 + quarter_len / sample_rate * 2.5) as f32;
    let outcome = capture_to_disk(state.debug_tap(), seconds, &capture_root())
        .expect("a playing pattern must be capturable");
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
    assert!(channel.peak > 0.1, "the pattern must be audible");

    let left = outcome.capture.channel(0);

    // --- the drums are still on the grid ------------------------------------
    let kicks = trim_warm_up(
        &detect_onsets(&left, sample_rate, &OnsetConfig::low(KICK_BAND_HZ)),
        quarter_len,
    );
    let kick_grid = align_grid(&kicks, quarter_len, sample_rate);
    report("kick", &kicks, &kick_grid, sample_rate);
    assert!(
        kicks.len() >= BARS * 4,
        "two bars of four-on-the-floor is at least eight kicks, got {}",
        kicks.len()
    );
    assert!(kick_grid.is_contiguous(), "a quarter is missing its kick");
    assert!(
        kick_grid.within(TOLERANCE_MS),
        "the kick drifted off the grid by {:.3} ms",
        kick_grid.max_abs_error_ms
    );

    // --- the melody, one quarter-note window at a time ----------------------
    //
    // The kick onsets are the grid. Each window is the middle of the quarter
    // that starts at one of them, which is the only part of it with exactly one
    // note sounding at its sustain level.
    let windows: Vec<(usize, f64, f64)> = kicks
        .iter()
        .filter_map(|onset| {
            let from = onset.frame + (quarter_len * WINDOW_FROM) as usize;
            let to = onset.frame + (quarter_len * WINDOW_TO) as usize;
            if to > left.len() {
                return None;
            }
            let (hz, bin_hz) = dominant_hz(&left[from..to], sample_rate)?;
            Some((onset.frame, hz, bin_hz))
        })
        .take(BARS * 4)
        .collect();
    assert_eq!(
        windows.len(),
        BARS * 4,
        "eight quarters had to fit in the capture; got {}",
        windows.len()
    );

    // Which quarter of the figure the capture started on. Recovered rather than
    // assumed, because arming the tap and starting the transport are different
    // events and nothing relates their phase. The wrong rotation is not a near
    // miss -- the four candidates are a major third and a fifth apart.
    let error_for = |rotation: usize| -> f64 {
        windows
            .iter()
            .enumerate()
            .map(|(index, (_, hz, _))| {
                let expected = key_hz(ARPEGGIO[(index + rotation) % ARPEGGIO.len()]);
                (hz - expected).abs()
            })
            .sum()
    };
    let rotation = (0..ARPEGGIO.len())
        .min_by(|a, b| error_for(*a).total_cmp(&error_for(*b)))
        .expect("four candidates");
    eprintln!(
        "alignment: the capture starts on quarter {} of the figure (total error {:.2} Hz; \
         the next best is {:.2} Hz)",
        rotation + 1,
        error_for(rotation),
        (0..ARPEGGIO.len())
            .filter(|r| *r != rotation)
            .map(error_for)
            .fold(f64::INFINITY, f64::min),
    );

    eprintln!("window |  key | expected Hz | measured Hz |   err Hz | err bins");
    eprintln!("-------|-----|-------------|-------------|----------|---------");
    let mut measurements = Vec::new();
    for (index, (frame, hz, bin_hz)) in windows.iter().enumerate() {
        let key = ARPEGGIO[(index + rotation) % ARPEGGIO.len()];
        let expected = key_hz(key);
        let bins = (hz - expected) / bin_hz;
        eprintln!(
            "{:>6} | {:>3} | {expected:>11.2} | {hz:>11.2} | {:>8.2} | {bins:>+8.2}",
            index + 1,
            key,
            hz - expected,
        );
        measurements.push(serde_json::json!({
            "window": index + 1,
            "frame": frame,
            "key": key,
            "expectedHz": expected,
            "measuredHz": hz,
            "errorHz": hz - expected,
            "errorBins": bins,
            "binHz": bin_hz,
        }));
    }

    for (index, (_, hz, bin_hz)) in windows.iter().enumerate() {
        let key = ARPEGGIO[(index + rotation) % ARPEGGIO.len()];
        let expected = key_hz(key);
        assert!(
            (hz - expected).abs() <= *bin_hz,
            "window {}: expected {key} ({expected:.2} Hz) but the dominant partial is \
             {hz:.2} Hz, which is {:.2} bins away ({bin_hz:.2} Hz per bin)",
            index + 1,
            (hz - expected) / bin_hz,
        );
    }

    // --- leave the numbers on disk ------------------------------------------
    let pitch_json = outcome.artifacts.dir.join("pitch.json");
    let payload = serde_json::json!({
        "bpm": BPM,
        "sampleRate": sample_rate,
        "bars": BARS,
        "quarterSamples": quarter_len,
        "toleranceMs": TOLERANCE_MS,
        "band": [PITCH_BAND_LOW_HZ, PITCH_BAND_HIGH_HZ],
        "windowFraction": [WINDOW_FROM, WINDOW_TO],
        "rotation": rotation,
        "kick": kick_grid,
        "pitches": measurements,
    });
    std::fs::write(&pitch_json, serde_json::to_vec_pretty(&payload).unwrap())
        .expect("the capture directory exists");
    eprintln!("pitch {}", pitch_json.display());

    // --- Stop must not leave a note hanging ---------------------------------
    //
    // The melodic half of the scheduler owes a note-off for every note it has
    // started, and the poly synth -- unlike the drum kit -- acts on them. This
    // is the assertion that a two-bar arpeggio does not end the session with
    // eight voices sounding.
    sleep(Duration::from_millis(600));
    let after_stop = capture_to_disk(state.debug_tap(), 0.25, &capture_root())
        .expect("the engine is still running");
    assert_eq!(
        after_stop.stats.per_channel[0].peak, 0.0,
        "the pattern must fall silent when the transport stops"
    );

    state.meters().shutdown();
    state.audio().stop();
}
