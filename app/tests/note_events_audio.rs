//! End-to-end proof that note events make sound, measured through the device.
//!
//! Everything except the webview: boot the kernel, load the fixture synth as the
//! graph's source, open the real output device, send notes through the real RT
//! note queue, and *capture what came out* with `debug-tap`. The assertions are
//! spectral -- "the dominant partial is 440 Hz", "the capture contains peaks at
//! 261.6, 329.6 and 392 Hz" -- because that is the only kind of evidence that
//! distinguishes "a note reached the plugin" from "something made a noise".
//!
//! `#[ignore]`d: it opens the audio device and is audible, so
//! `cargo test --workspace` must stay silent and pass on a machine with no
//! sound card.
//!
//! ```sh
//! cargo test -p mydaw-app --test note_events_audio -- --ignored --nocapture
//! ```
//!
//! Every run leaves its artefacts (WAV, annotated PNG, stats JSON) under
//! `.debug/captures/<timestamp>/` and prints the paths, so a failure can be
//! looked at rather than guessed at.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use mydaw_app::{boot_kernel, capture_root, capture_to_disk, AppState, CaptureOutcome};
use mydaw_fixture_clap_gain::synth::{key_to_hz, PLUGIN_ID as SYNTH_ID, RELEASE_MS, VOICE_GAIN};
use mydaw_plugin_debug_tap::ChannelStats;
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_host_clap::LoadedClapPlugin;

/// How long each measured capture runs. One second is ~44 100 frames, which
/// gives the FFT enough resolution to separate the notes of a triad.
const CAPTURE_SECONDS: f32 = 1.0;

/// Tolerance on a measured fundamental, in Hz.
///
/// The capture FFT's bin spacing at these lengths is around 1.5 Hz and the peak
/// is parabolically interpolated, so 3 Hz is comfortable without being blind to
/// a real mistuning (a semitone at A4 is 26 Hz).
const HZ_TOLERANCE: f64 = 3.0;

/// A scratch directory that deletes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("mydaw-notes-e2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
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

/// The whole rig: kernel, shell state, and the synth as the graph's source.
///
/// Returns `None` (having said why) when the fixture was not built or the
/// machine has no output device, so the test skips rather than fails.
struct Rig {
    state: AppState,
    plugin: LoadedClapPlugin,
    _kernel: mydaw_kernel::Kernel,
    _scratch: TempDir,
}

fn rig(tag: &str) -> Option<Rig> {
    let scratch = TempDir::new(tag);
    let bundle = match stage_fixture_bundle(scratch.path()) {
        Some(bundle) => bundle,
        None => {
            eprintln!("{}", missing_fixture_message());
            return None;
        }
    };

    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("the synth fixture loads");
    assert!(
        plugin.port_layout().has_note_input(),
        "the fixture must declare a note input, or this test proves nothing"
    );

    match state.start_audio_with_plugins(Some(&mut plugin), None) {
        Ok(info) => eprintln!(
            "device: {} @ {} Hz, {} ch (max block {}); instrument '{}' active={}",
            info.device_name,
            info.sample_rate,
            info.channels,
            info.max_block,
            plugin.info().id,
            plugin.is_active()
        ),
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            return None;
        }
    }
    assert!(
        plugin.is_active(),
        "the instrument must be in the live graph"
    );

    Some(Rig {
        state,
        plugin,
        _kernel: kernel,
        _scratch: scratch,
    })
}

impl Rig {
    /// Capture the engine's output and report where the artefacts landed.
    fn capture(&self, label: &str, seconds: f32) -> CaptureOutcome {
        let outcome = capture_to_disk(self.state.debug_tap(), seconds, &capture_root())
            .unwrap_or_else(|err| panic!("capture '{label}' failed: {err}"));
        let stats = &outcome.stats;
        eprintln!(
            "[{label}] {} frames @ {} Hz | L peak {:.4} ({:.2} dBFS) rms {:.4} | dominant {:?} Hz | flatness {:.4}",
            stats.frames,
            stats.sample_rate,
            stats.per_channel[0].peak,
            stats.per_channel[0].peak_dbfs,
            stats.per_channel[0].rms,
            stats.per_channel[0].dominant_hz,
            stats.per_channel[0].spectral_flatness,
        );
        eprintln!("[{label}] png  {}", outcome.artifacts.png.display());
        eprintln!("[{label}] json {}", outcome.artifacts.stats_json.display());
        assert_eq!(
            outcome.capture.dropped, 0,
            "[{label}] the capture has holes; its measurements cannot be trusted"
        );
        assert_eq!(
            stats.per_channel[0].non_finite, 0,
            "[{label}] a NaN or infinity reached the device"
        );
        outcome
    }

    fn tear_down(mut self) {
        self.state.meters().shutdown();
        self.state.audio().stop();
        assert!(
            self.plugin.shutdown(),
            "the instrument must deactivate on the main thread after the graph is retired"
        );
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
#[ignore = "opens the audio device and plays audible notes"]
fn a_note_on_sounds_at_the_key_frequency_and_a_note_off_decays_to_silence() {
    let Some(rig) = rig("single") else { return };

    // --- before any note: an instrument makes no sound on its own ---------
    // Transport deliberately left stopped: the instrument chain has no gate,
    // and pressing a key must work without pressing Play first.
    let idle = rig.capture("idle", 0.2);
    assert_eq!(
        idle.stats.per_channel[0].peak, 0.0,
        "an instrument with no notes held must be digitally silent"
    );

    // --- note on: A4 -------------------------------------------------------
    rig.state.note_on(69, 1.0).expect("the note queue accepts");
    // Skip the 2 ms attack ramp so the capture measures the steady state.
    sleep(Duration::from_millis(50));
    let sounding = rig.capture("note-on-a4", CAPTURE_SECONDS);

    for channel in &sounding.stats.per_channel {
        let dominant = channel
            .dominant_hz
            .unwrap_or_else(|| panic!("channel {} has no spectral peak at all", channel.label));
        assert!(
            (dominant - f64::from(key_to_hz(69))).abs() <= HZ_TOLERANCE,
            "channel {}: key 69 should sound at 440 Hz, measured {dominant:.2} Hz",
            channel.label
        );
        assert!(
            (channel.peak - VOICE_GAIN).abs() < 0.02,
            "channel {}: velocity 1.0 should peak at {VOICE_GAIN}, got {}",
            channel.label,
            channel.peak
        );
        assert!(
            channel.spectral_flatness < 0.1,
            "channel {}: a single sine must read as tonal, got flatness {}",
            channel.label,
            channel.spectral_flatness
        );
        assert_eq!(channel.clipped, 0);
    }

    // --- note off: silence within the release ------------------------------
    rig.state.note_off(69).expect("the note queue accepts");
    // The release is 50 ms; wait several times that before asking for silence,
    // so this measures "the voice ended", not "the ramp is still running".
    sleep(Duration::from_millis((RELEASE_MS * 4.0) as u64));
    let released = rig.capture("note-off", 0.3);
    assert_eq!(
        released.stats.per_channel[0].peak, 0.0,
        "the voice must be digitally silent {RELEASE_MS} ms after its note-off"
    );
    assert_eq!(released.stats.per_channel[1].peak, 0.0);

    rig.tear_down();
}

#[test]
#[ignore = "opens the audio device and plays an audible chord"]
fn a_chord_puts_every_one_of_its_notes_into_the_spectrum() {
    let Some(rig) = rig("chord") else { return };

    // C major: C4, E4, G4.
    const KEYS: [u8; 3] = [60, 64, 67];
    for key in KEYS {
        rig.state.note_on(key, 1.0).expect("the note queue accepts");
    }
    sleep(Duration::from_millis(50));
    let chord = rig.capture("chord-c-major", CAPTURE_SECONDS);

    let channel = &chord.stats.per_channel[0];
    eprintln!(
        "[chord-c-major] top peaks: {:?}",
        channel
            .top_bins
            .iter()
            .map(|p| (p.frequency_hz.round(), p.magnitude_dbfs.round()))
            .collect::<Vec<_>>()
    );

    for key in KEYS {
        let expected = f64::from(key_to_hz(key));
        assert!(
            has_peak_near(channel, expected),
            "key {key} ({expected:.1} Hz) is missing from the spectrum; peaks were {:?}",
            channel
                .top_bins
                .iter()
                .map(|p| p.frequency_hz)
                .collect::<Vec<_>>()
        );
    }

    // Three voices, so meaningfully louder than one but nowhere near clipping.
    assert!(
        channel.peak > VOICE_GAIN * 1.2,
        "three voices should be louder than one, got {}",
        channel.peak
    );
    assert_eq!(channel.clipped, 0, "the chord must not clip");

    for key in KEYS {
        rig.state.note_off(key).expect("the note queue accepts");
    }
    sleep(Duration::from_millis((RELEASE_MS * 4.0) as u64));
    let released = rig.capture("chord-off", 0.3);
    assert_eq!(
        released.stats.per_channel[0].peak, 0.0,
        "every voice must release"
    );

    rig.tear_down();
}
