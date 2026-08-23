//! End-to-end proof that the built-in instruments make sound, measured through
//! the device.
//!
//! Everything except the webview: boot the kernel, install a built-in
//! instrument as the graph's source, open the real output device, send notes
//! through the real RT note queue, and *capture what came out* with
//! `debug-tap`. The assertions are spectral -- "the dominant partial is 440 Hz",
//! "the kick's energy is below 100 Hz", "the snare is broadband" -- because that
//! is the only kind of evidence that distinguishes "the instrument is in the
//! graph" from "something made a noise".
//!
//! It is the same shape as `note_events_audio.rs`, which does this for a hosted
//! CLAP instrument. The two exist side by side on purpose: one proves the
//! plugin path, this one proves the internal path, and neither substitutes for
//! the other.
//!
//! `#[ignore]`d: it opens the audio device and is audible, so
//! `cargo test --workspace` must stay silent and pass on a machine with no
//! sound card.
//!
//! ```sh
//! cargo test -p mydaw-app --test instrument_audio -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every run leaves its artefacts (WAV, annotated PNG, stats JSON) under
//! `.debug/captures/<timestamp>/` and prints the paths, so a failure can be
//! looked at rather than guessed at.

use std::thread::sleep;
use std::time::Duration;

use mydaw_app::{boot_kernel, capture_root, capture_to_disk, AppState, CaptureOutcome, Source};
use mydaw_plugin_debug_tap::ChannelStats;
use mydaw_plugin_instruments::key_to_hz;

/// One second is ~44 100 frames, which gives the FFT enough resolution to
/// separate a sawtooth's low harmonics.
const CAPTURE_SECONDS: f32 = 1.0;

/// Tolerance on a measured partial, in Hz. The capture FFT's bin spacing at
/// these lengths is around 1.5 Hz and peaks are parabolically interpolated, so
/// 3 Hz is comfortable without being blind to a real mistuning (a semitone at
/// A4 is 26 Hz).
const HZ_TOLERANCE: f64 = 3.0;

/// The rig: kernel, shell state, and a built-in instrument as the source.
///
/// Returns `None` (having said why) when the machine has no output device, so
/// the test skips rather than fails.
struct Rig {
    state: AppState,
    _kernel: mydaw_kernel::Kernel,
}

fn rig(source: Source) -> Option<Rig> {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");

    match state.start_audio_with_source(source.clone()) {
        Ok(info) => eprintln!(
            "device: {} @ {} Hz, {} ch (max block {}); source '{}'",
            info.device_name,
            info.sample_rate,
            info.channels,
            info.max_block,
            state.current_source().id()
        ),
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            return None;
        }
    }
    assert_eq!(
        state.current_source(),
        source,
        "the requested source must be the one installed"
    );

    Some(Rig {
        state,
        _kernel: kernel,
    })
}

impl Rig {
    /// Capture the engine's output and report where the artefacts landed.
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

/// Whether `stats` has a spectral peak within [`HZ_TOLERANCE`] of `hz`.
fn has_peak_near(stats: &ChannelStats, hz: f64) -> bool {
    stats
        .top_bins
        .iter()
        .any(|peak| (peak.frequency_hz - hz).abs() <= HZ_TOLERANCE)
}

#[test]
#[ignore = "opens the audio device and plays an audible note"]
fn the_poly_synth_sounds_at_the_key_it_is_given_and_releases_to_silence() {
    let Some(rig) = rig(Source::PolySynth) else {
        return;
    };

    // --- before any note ---------------------------------------------------
    // Transport deliberately left stopped: an instrument has no gate, and
    // pressing a key must work without pressing Play first.
    let idle = rig.capture("poly-idle", 0.2);
    assert_eq!(
        idle.stats.per_channel[0].peak, 0.0,
        "an instrument with no notes held must be digitally silent"
    );

    // --- note on: A4 -------------------------------------------------------
    rig.state.note_on(69, 1.0).expect("the note queue accepts");
    // Skip the 5 ms attack and the 80 ms decay so the capture measures sustain.
    sleep(Duration::from_millis(150));
    let sounding = rig.capture("poly-a4", CAPTURE_SECONDS);

    for channel in &sounding.stats.per_channel {
        let dominant = channel
            .dominant_hz
            .unwrap_or_else(|| panic!("channel {} has no spectral peak at all", channel.label));
        assert!(
            (dominant - f64::from(key_to_hz(69))).abs() <= HZ_TOLERANCE,
            "channel {}: key 69 should sound at 440 Hz, measured {dominant:.2} Hz",
            channel.label
        );
        // A saw has every harmonic; these two are the ones the 3 kHz lowpass
        // leaves unmistakable. 220 Hz is the sub-oscillator.
        for partial in [880.0, 1_320.0, 220.0] {
            assert!(
                has_peak_near(channel, partial),
                "channel {}: {partial} Hz is missing; peaks were {:?}",
                channel.label,
                channel
                    .top_bins
                    .iter()
                    .map(|peak| peak.frequency_hz)
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            channel.spectral_flatness < 0.2,
            "channel {}: a held note must read as tonal, got flatness {}",
            channel.label,
            channel.spectral_flatness
        );
        assert_eq!(channel.clipped, 0, "one note must not clip");
    }

    // --- note off: silence within the release ------------------------------
    rig.state.note_off(69).expect("the note queue accepts");
    // The release is 120 ms; wait several times that before asking for silence,
    // so this measures "the voice ended", not "the ramp is still running".
    sleep(Duration::from_millis(600));
    let released = rig.capture("poly-release", 0.3);
    assert_eq!(
        released.stats.per_channel[0].peak, 0.0,
        "the voice must be digitally silent after its release"
    );
    assert_eq!(released.stats.per_channel[1].peak, 0.0);

    rig.tear_down();
}

#[test]
#[ignore = "opens the audio device and plays an audible chord"]
fn a_chord_on_the_poly_synth_puts_every_note_into_the_spectrum() {
    let Some(rig) = rig(Source::PolySynth) else {
        return;
    };

    const KEYS: [u8; 3] = [60, 64, 67]; // C major
    for key in KEYS {
        rig.state.note_on(key, 1.0).expect("the note queue accepts");
    }
    sleep(Duration::from_millis(150));
    let chord = rig.capture("poly-chord", CAPTURE_SECONDS);

    let channel = &chord.stats.per_channel[0];
    for key in KEYS {
        let expected = f64::from(key_to_hz(key));
        assert!(
            has_peak_near(channel, expected),
            "key {key} ({expected:.1} Hz) is missing from the spectrum"
        );
    }
    assert_eq!(channel.clipped, 0, "a triad must not clip");

    for key in KEYS {
        rig.state.note_off(key).expect("the note queue accepts");
    }
    sleep(Duration::from_millis(600));
    let released = rig.capture("poly-chord-off", 0.3);
    assert_eq!(
        released.stats.per_channel[0].peak, 0.0,
        "every voice must release"
    );

    rig.tear_down();
}
