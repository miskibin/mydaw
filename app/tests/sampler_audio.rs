//! The sampled piano, measured through the sound card.
//!
//! `plugins/sampler/tests/voices.rs` proves the arithmetic offline against
//! synthetic content, and `plugins/sampler/tests/piano_assets.rs` proves the
//! real recordings decode and map correctly. This proves the last link neither
//! of them can: that setting a track's source to the piano **through the
//! command the UI calls** puts it in the running graph, and that what leaves the
//! device is a piano.
//!
//! Three things are verified, each for a different reason:
//!
//! 1. **A chord.** Three fundamentals in the spectrum at once says the voice
//!    allocator, the lookup and the mixer are all doing their jobs. Its decay
//!    says the *content* arrived -- a synthesised fallback would sustain.
//! 2. **Velocity.** Two notes at different velocities differ in level, which is
//!    the end-to-end version of the layer-selection unit tests.
//! 3. **A melody, from the piano roll.** The step-9 machinery, unchanged, with
//!    the synth swapped for the sampler: a pattern of notes in ticks, captured,
//!    and every quarter-note window's dominant partial compared against the note
//!    the pattern says is sounding there.
//!
//! Skips itself twice over: `#[ignore]`d because it opens the audio device and
//! is audible, and it returns early when the sample content is not installed.
//!
//! ```sh
//! pwsh -File scripts/fetch-assets.ps1
//! cargo test -p mydaw-app --test sampler_audio -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every run leaves a WAV, an annotated PNG and a stats JSON under
//! `.debug/captures/<timestamp>/`, so a failure can be looked at rather than
//! guessed at.

use std::thread::sleep;
use std::time::{Duration, Instant};

use mydaw_app::{
    boot_kernel, capture_root, capture_to_disk, AppState, CaptureOutcome, MelodicNoteDto,
    PatternDto, PatternRowDto, SamplerSourceRef, Source, StepDto,
};
use mydaw_plugin_debug_tap::{align_grid, detect_onsets, spectrum, Band, OnsetConfig};
use mydaw_plugin_sequencer::{step_len_samples, DEFAULT_STEP_VELOCITY, TICKS_PER_STEP};

/// The instrument under test.
const INSTRUMENT: &str = "salamander-piano";
const SOURCE_ID: &str = "sampler:salamander-piano";

/// Longest a first scan may take before the test gives up. Decoding 222 MB of
/// FLAC is about nine seconds on a warm machine; this is a stall, not a budget.
const SCAN_TIMEOUT: Duration = Duration::from_secs(180);

/// Tolerance on a measured partial, in Hz. Wider than the synth's 3 Hz: a piano
/// string is slightly inharmonic and the recordings carry real room tone, so a
/// peak lands a fraction of a percent off its ideal. A semitone at C4 is 15 Hz,
/// so this still cannot confuse two notes.
const HZ_TOLERANCE: f64 = 4.0;

/// C major, the chord the whole spectrum assertion is about.
const CHORD: [u8; 3] = [60, 64, 67];

// --- the melody -------------------------------------------------------------
// The step-9 constants, unchanged. Anything different here would make the pitch
// table incomparable with the one in that spec, which is the point of reusing it.

const BPM: f64 = 130.0;
const BARS: usize = 2;
const STEPS: usize = BARS * 16;
const QUARTER_TICKS: u32 = TICKS_PER_STEP * 4;
/// Track 1 of the default project, which this test re-heads with the piano. A
/// track's index *is* its note channel.
const PIANO_CHANNEL: u8 = 1;
const ARPEGGIO: [u8; 4] = [60, 64, 67, 72];
const SETTLE: Duration = Duration::from_millis(500);
const KICK_BAND_HZ: f32 = 120.0;
const TOLERANCE_MS: f64 = 1.0;
/// Band the pitch is measured in. The low corner removes the kick; the high
/// corner removes the hats and the piano's own upper partials, which in the bass
/// can be louder than the fundamental.
const PITCH_BAND_LOW_HZ: f32 = 200.0;
const PITCH_BAND_HIGH_HZ: f32 = 1_500.0;
/// The part of each quarter-note window that is analysed. Later than step 9's
/// 0.40--0.80: a piano's hammer transient is broadband and lasts longer than a
/// synth's 5 ms attack, so the first part of the window is not yet a pitch.
const WINDOW_FROM: f64 = 0.45;
const WINDOW_TO: f64 = 0.85;

/// Equal-tempered frequency of a MIDI key, A4 = 440 Hz.
fn key_hz(key: u8) -> f64 {
    440.0 * 2.0_f64.powf((f64::from(key) - 69.0) / 12.0)
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// Wait for the sampler's background scan, and say whether the piano arrived.
///
/// The scan starts at plugin boot and takes seconds; a test that asked for the
/// source immediately would race it. Returns `false` as soon as a *completed*
/// scan is known not to have found the instrument, so a machine with no assets
/// skips at once rather than waiting out the timeout.
fn wait_for_piano(state: &AppState) -> bool {
    let registry = state.sampler();
    let deadline = Instant::now() + SCAN_TIMEOUT;
    while Instant::now() < deadline {
        if registry.contains(SOURCE_ID) {
            return true;
        }
        if !registry.is_scanning() && registry.generation() > 0 {
            eprintln!(
                "skipping: '{INSTRUMENT}' is not installed under {} -- \
                 run scripts/fetch-assets.ps1",
                registry.root().display()
            );
            return false;
        }
        sleep(Duration::from_millis(200));
    }
    eprintln!("skipping: the sampler scan did not finish within {SCAN_TIMEOUT:?}");
    false
}

/// Kernel, shell state, and the piano at the head of the chain.
struct Rig {
    state: AppState,
    _kernel: mydaw_kernel::Kernel,
}

fn rig() -> Option<Rig> {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");
    if !wait_for_piano(&state) {
        return None;
    }

    let info = state.sampler().info(SOURCE_ID).expect("just checked");
    eprintln!(
        "instrument: {} | {} samples ({} skipped) | {:.1} MB decoded",
        info.name,
        info.samples,
        info.skipped,
        info.bytes as f64 / 1e6
    );
    eprintln!("attribution: {}", info.attribution);

    let source = Source::Sampler(SamplerSourceRef::new(INSTRUMENT));
    match state.start_audio_with_source(source.clone()) {
        Ok(device) => eprintln!(
            "device: {} @ {} Hz, {} ch (max block {}); source '{}'",
            device.device_name,
            device.sample_rate,
            device.channels,
            device.max_block,
            state.current_source().id()
        ),
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            return None;
        }
    }
    assert_eq!(
        state.current_source().id(),
        SOURCE_ID,
        "the requested source must be the one installed"
    );

    Some(Rig {
        state,
        _kernel: kernel,
    })
}

impl Rig {
    fn capture(&self, label: &str, seconds: f32) -> CaptureOutcome {
        let outcome = capture_to_disk(self.state.debug_tap(), seconds, &capture_root())
            .unwrap_or_else(|err| panic!("capture '{label}' failed: {err}"));
        let stats = &outcome.stats;
        let channel = &stats.per_channel[0];
        eprintln!(
            "[{label}] {} frames @ {} Hz | L peak {:.4} ({:.2} dBFS) rms {:.4} | dominant {:?} Hz | flatness {:.4}",
            stats.frames,
            stats.sample_rate,
            channel.peak,
            channel.peak_dbfs,
            channel.rms,
            channel.dominant_hz,
            channel.spectral_flatness,
        );
        eprintln!(
            "[{label}] top peaks {:?}",
            channel
                .top_bins
                .iter()
                .map(|peak| (peak.frequency_hz.round(), peak.magnitude_dbfs.round()))
                .collect::<Vec<_>>()
        );
        eprintln!("[{label}] png  {}", outcome.artifacts.png.display());
        eprintln!("[{label}] json {}", outcome.artifacts.stats_json.display());
        assert_eq!(
            outcome.capture.dropped, 0,
            "[{label}] the capture has holes; its measurements cannot be trusted"
        );
        assert_eq!(
            channel.non_finite, 0,
            "[{label}] a NaN or infinity reached the device"
        );
        outcome
    }

    fn tear_down(self) {
        self.state.meters().shutdown();
        self.state.audio().stop();
    }
}

/// Deviation of `hz` from `expected`, in cents, folded into ±600.
///
/// # Why the fold
/// A real piano string's **second harmonic is routinely louder than its
/// fundamental** -- markedly so on this instrument, where a captured C4 shows
/// 523 Hz some 13 dB above 261 Hz. That is a property of a struck string and of
/// where the microphones were, not of the sampler: `plugins/sampler/tests`
/// measures the pitch ratio exactly, on synthetic content where there *is* only
/// one partial.
///
/// So through the device the honest question is "is this the right note?", not
/// "is the loudest bin the fundamental?" -- and the answer is a pitch class.
/// Folding by octaves answers it while still catching every error that matters:
/// a semitone is 100 cents, and no wrong note in this pattern is an octave from
/// a right one.
fn chroma_cents(hz: f64, expected: f64) -> f64 {
    let cents = 1200.0 * (hz / expected).log2();
    cents - 1200.0 * (cents / 1200.0).round()
}

/// Worst pitch-class deviation tolerated, in cents. A third of a semitone.
const CENTS_TOLERANCE: f64 = 35.0;

#[test]
#[ignore = "opens the audio device and plays an audible piano chord"]
fn a_c_major_chord_leaves_the_device_with_all_three_notes_and_a_piano_decay() {
    let Some(rig) = rig() else { return };

    // An instrument is not gated on the transport: a key must sound without
    // pressing Play first.
    let idle = rig.capture("piano-idle", 0.2);
    assert_eq!(
        idle.stats.per_channel[0].peak, 0.0,
        "a piano with no keys held must be digitally silent"
    );

    for key in CHORD {
        rig.state.note_on(key, 1.0).expect("the note queue accepts");
    }
    // Past the hammer transient, into the part that has a pitch.
    sleep(Duration::from_millis(120));
    let chord = rig.capture("piano-chord", 2.5);

    // --- all three notes are there ------------------------------------------
    // Measured from the capture rather than from `stats.top_bins`, which keeps
    // only the loudest five: three fundamentals and three second harmonics are
    // six, so a triad of a harmonically rich instrument does not fit.
    let channel = &chord.stats.per_channel[0];
    let left = chord.capture.channel(0);
    let sample_rate = chord.stats.sample_rate;
    let peaks = spectrum(&left, sample_rate).top_peaks(24);
    eprintln!("--- chord partials ---");
    for key in CHORD {
        let expected = key_hz(key);
        let found = peaks
            .iter()
            .find(|peak| (peak.frequency_hz - expected).abs() <= HZ_TOLERANCE);
        eprintln!(
            "  key {key:>3} expected {expected:>8.2} Hz -> {}",
            found.map_or("MISSING".to_owned(), |peak| format!(
                "{:8.2} Hz at {:>6.1} dBFS",
                peak.frequency_hz, peak.magnitude_dbfs
            ))
        );
        assert!(
            found.is_some(),
            "key {key} ({expected:.2} Hz) is missing; peaks were {:?}",
            peaks
                .iter()
                .map(|peak| peak.frequency_hz.round())
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(channel.clipped, 0, "a triad must not clip");
    assert!(channel.peak > 0.02, "the chord must be audible");
    assert!(
        channel.spectral_flatness < 0.3,
        "a piano chord must read as tonal, got flatness {}",
        channel.spectral_flatness
    );

    // --- and it decays, which is what says the recordings arrived ------------
    // A synthesised fallback would sustain; a piano cannot.
    let window = left.len() / 5;
    let levels: Vec<f32> = (0..5)
        .map(|i| rms(&left[i * window..(i + 1) * window]))
        .collect();
    eprintln!("--- decay (fifths of the capture) ---");
    for (index, level) in levels.iter().enumerate() {
        eprintln!(
            "  {index}  rms {level:.5}  ({:.1} dBFS)",
            20.0 * level.max(1e-9).log10()
        );
    }
    assert!(
        levels[0] > levels[4] * 2.0,
        "a piano must decay: the capture went {levels:?}"
    );
    assert!(
        levels.windows(2).all(|pair| pair[1] < pair[0] * 1.05),
        "the decay must be monotone within measurement noise: {levels:?}"
    );

    for key in CHORD {
        rig.state.note_off(key).expect("the note queue accepts");
    }
    // The manifest's release is 400 ms; wait several times that.
    sleep(Duration::from_millis(1_500));
    let released = rig.capture("piano-release", 0.3);
    assert_eq!(
        released.stats.per_channel[0].peak, 0.0,
        "every voice must release to digital silence"
    );

    rig.tear_down();
}

#[test]
#[ignore = "opens the audio device and plays two audible piano notes"]
fn a_soft_note_and_a_hard_one_differ_in_level_through_the_device() {
    let Some(rig) = rig() else { return };

    let level = |label: &str, velocity: f32| {
        rig.state.note_on(60, velocity).expect("queue accepts");
        sleep(Duration::from_millis(120));
        let outcome = rig.capture(label, 1.0);
        rig.state.note_off(60).expect("queue accepts");
        sleep(Duration::from_millis(1_200));
        outcome.stats.per_channel[0].rms
    };

    let soft = level("piano-vel-030", 0.3);
    let hard = level("piano-vel-100", 1.0);
    let contrast = 20.0 * (hard / soft.max(1e-9)).log10();
    eprintln!("--- velocity ---");
    eprintln!(
        "  0.3  rms {soft:.5} ({:.1} dBFS)",
        20.0 * soft.max(1e-9).log10()
    );
    eprintln!(
        "  1.0  rms {hard:.5} ({:.1} dBFS)",
        20.0 * hard.max(1e-9).log10()
    );
    eprintln!("  contrast {contrast:.1} dB");

    assert!(soft > 0.0, "even a soft note must be audible");
    assert!(
        contrast > 6.0,
        "velocity 0.3 and 1.0 must be clearly different; got {contrast:.1} dB"
    );

    rig.tear_down();
}

/// The two-bar pattern, in the shape the webview sends. Step 9's, with the
/// melody aimed at whichever track this test re-heads with the piano.
fn arpeggio_over_a_beat() -> PatternDto {
    let step = |on: bool| StepDto {
        on,
        velocity: DEFAULT_STEP_VELOCITY,
    };
    let row = |key: u8, hit: &dyn Fn(usize) -> bool| PatternRowDto {
        key,
        channel: 0,
        label: String::new(),
        steps: (0..STEPS).map(|index| step(hit(index))).collect(),
    };

    PatternDto {
        steps: STEPS as u8,
        rows: vec![
            row(36, &|index| index % 4 == 0),
            row(38, &|_| false),
            row(42, &|index| index % 2 == 0),
            row(46, &|_| false),
        ],
        notes: (0..BARS * 4)
            .map(|quarter| MelodicNoteDto {
                key: ARPEGGIO[quarter % ARPEGGIO.len()],
                channel: PIANO_CHANNEL,
                start: quarter as u32 * QUARTER_TICKS,
                length: QUARTER_TICKS,
                velocity: DEFAULT_STEP_VELOCITY,
            })
            .collect(),
    }
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
#[ignore = "opens the audio device and plays an audible piano arpeggio over a beat"]
fn a_melody_from_the_piano_roll_comes_out_of_the_device_in_tune_on_the_piano() {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");
    if !wait_for_piano(&state) {
        return;
    }

    // The default project: track 0 the drum kit, track 1 the poly synth. Re-head
    // track 1 with the piano **through the command the mixer's picker calls**,
    // which is the integration this test exists to prove.
    let info = match state.start_audio_with_project() {
        Ok(info) => info,
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            return;
        }
    };
    state
        .set_track_source(
            PIANO_CHANNEL as usize,
            Source::Sampler(SamplerSourceRef::new(INSTRUMENT)),
        )
        .expect("the piano must install on a running graph");
    eprintln!(
        "device: {} @ {} Hz, {} ch (max block {}); track {PIANO_CHANNEL} source '{}'",
        info.device_name,
        info.sample_rate,
        info.channels,
        info.max_block,
        state.tracks().project().tracks[PIANO_CHANNEL as usize]
            .source
            .id()
    );
    assert_eq!(
        state.tracks().project().tracks[PIANO_CHANNEL as usize]
            .source
            .id(),
        SOURCE_ID
    );

    let sample_rate = f64::from(info.sample_rate);
    let step_len = step_len_samples(sample_rate, BPM);
    let quarter_len = step_len * 4.0;
    let bar_seconds = step_len * 16.0 / sample_rate;

    assert_eq!(state.set_bpm(BPM).expect("the tempo is legal"), BPM);
    state
        .set_pattern_dto(&arpeggio_over_a_beat())
        .expect("the pattern is valid");

    state.set_playing(true).expect("the transport starts");
    sleep(SETTLE);

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

    assert_eq!(outcome.capture.dropped, 0, "the capture has holes");
    assert_eq!(channel.non_finite, 0, "a NaN reached the device");
    assert!(channel.peak > 0.05, "the pattern must be audible");
    assert_eq!(channel.clipped, 0, "a piano over a beat must not clip");

    let left = outcome.capture.channel(0);

    // --- the drums define the grid ------------------------------------------
    let kicks = detect_onsets(&left, sample_rate, &OnsetConfig::low(KICK_BAND_HZ));
    // Drop a leading onset that is not a quarter from the next: the band
    // filter starts from zero state, and this capture always begins mid-loop.
    let kicks = {
        let mut trimmed = &kicks[..];
        while trimmed.len() >= 2 {
            let gap = (trimmed[1].frame - trimmed[0].frame) as f64;
            if (gap - quarter_len).abs() <= quarter_len * 0.05 {
                break;
            }
            trimmed = &trimmed[1..];
        }
        trimmed.to_vec()
    };
    let grid = align_grid(&kicks, quarter_len, sample_rate);
    eprintln!(
        "[kick] {} onsets | worst {:.3} ms | mean {:.3} ms | contiguous {}",
        kicks.len(),
        grid.max_abs_error_ms,
        grid.mean_abs_error_ms,
        grid.is_contiguous()
    );
    assert!(kicks.len() >= BARS * 4, "got {} kicks", kicks.len());
    assert!(grid.is_contiguous(), "a quarter is missing its kick");
    assert!(
        grid.within(TOLERANCE_MS),
        "the kick drifted off the grid by {:.3} ms",
        grid.max_abs_error_ms
    );

    // --- one pitch per quarter-note window ----------------------------------
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
        "every quarter of two bars must yield a measurable window"
    );

    // The capture starts at an unknowable phase of a four-note figure, so the
    // rotation is *recovered* rather than assumed: the alignment with the
    // smallest total error is chosen before anything is asserted. A pattern
    // playing the wrong notes fits none of the four.
    let best = (0..ARPEGGIO.len())
        .min_by(|a, b| {
            let error = |rotation: &usize| -> f64 {
                windows
                    .iter()
                    .enumerate()
                    .map(|(index, (_, hz, _))| {
                        let key = ARPEGGIO[(index + rotation) % ARPEGGIO.len()];
                        chroma_cents(*hz, key_hz(key)).abs()
                    })
                    .sum()
            };
            error(a).total_cmp(&error(b))
        })
        .expect("four rotations");

    eprintln!("--- per-window pitch (rotation {best}) ---");
    eprintln!(
        "{:>6}  {:>9}  {:>4}  {:>10}  {:>10}  {:>8}  {:>7}",
        "window", "t (s)", "key", "expected", "measured", "partial", "cents"
    );
    let mut worst_cents = 0.0_f64;
    for (index, (frame, hz, bin_hz)) in windows.iter().enumerate() {
        let key = ARPEGGIO[(index + best) % ARPEGGIO.len()];
        let expected = key_hz(key);
        let cents = chroma_cents(*hz, expected);
        // Which partial the measurement landed on. Printed rather than hidden:
        // "the second harmonic, in tune" and "the fundamental, in tune" are
        // different facts, and both are passes -- see `chroma_cents`.
        let partial = (hz / expected).log2().round().exp2();
        worst_cents = worst_cents.max(cents.abs());
        eprintln!(
            "{index:>6}  {:>9.4}  {key:>4}  {expected:>10.2}  {hz:>10.2}  {:>8}  {cents:>+7.1}",
            *frame as f64 / sample_rate,
            format!("x{partial:.0}")
        );
        // One FFT bin is ~11 Hz here, which at C4 is 70 cents. The tolerance is
        // the larger of the two, so the assertion is never tighter than the
        // measurement it is made from.
        let floor = 1200.0 * (1.0 + bin_hz / expected).log2();
        let tolerance = CENTS_TOLERANCE.max(floor);
        assert!(
            cents.abs() <= tolerance,
            "window {index}: key {key} ({expected:.2} Hz) measured {hz:.2} Hz, \
             {cents:+.1} cents out of tune (tolerance {tolerance:.1})"
        );
    }
    eprintln!("worst pitch-class deviation: {worst_cents:.1} cents");

    state.meters().shutdown();
    state.audio().stop();
}
