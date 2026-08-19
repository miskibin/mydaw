//! Hosted CLAP instruments, measured through the sound card.
//!
//! `app/tests/clap_instruments.rs` proves the plugins are right in an offline
//! render. This proves they are right *after the audio device has had them*: a
//! real WASAPI stream, a real callback at whatever block size the driver asked
//! for, real plugin instances placed through the real command mailbox, and
//! captures taken from the same tap the debug harness uses.
//!
//! **No environment variables.** Step 5 needed `MYDAW_CLAP_INSTRUMENT` to get a
//! plugin into the graph at all; every placement here goes through
//! [`ClapCommand`], which is exactly what the mixer strip's source dropdown
//! sends. That is the point of the step, so it is the point of this test.
//!
//! Three measurements, in one run:
//!
//! 1. **A hosted synth on track 1, alongside the beat on track 0.** The kick's
//!    band and the note's partial are both in the same capture, which is what
//!    "a plugin on a track" has to mean in a mixer.
//! 2. **A chord split across two instances.** Two tracks, two hosted synths,
//!    three notes between them -- every partial present, which one shared
//!    instance could not produce.
//! 3. **State: set, save, change, restore.** The fixture's state is a detune in
//!    semitones, so a state change *moves the pitch*: the capture shows C5 where
//!    C4 was, and shows C4 again once the saved blob is loaded back.
//!
//! `#[ignore]`d: it opens the audio device and is audible, so
//! `cargo test --workspace` must stay silent and pass on a machine with no
//! sound card.
//!
//! ```sh
//! cargo test -p mydaw-app --test clap_hardware -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every capture leaves a WAV, an annotated PNG and a stats JSON under
//! `.debug/captures/<timestamp>/`, and the paths are printed -- so a failure can
//! be looked at rather than guessed at.

use std::path::Path;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use mydaw_app::clap_bridge::{ClapCommand, ClapHostBridge, ClapTarget};
use mydaw_app::clap_host::ClapHost;
use mydaw_app::{boot_kernel, capture_root, capture_to_disk, AppState, CaptureOutcome, TrackHost};
use mydaw_fixture_clap_gain::synth::{self, PLUGIN_ID as SYNTH_ID};
use mydaw_plugin_audio_io::AudioIoService;
use mydaw_plugin_debug_tap::{spectrum, Spectrum};
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_sequencer::{Pattern, StepRow};
use mydaw_plugin_windows::PluginWindowService;

/// Tempo the beat runs at. The engine's default, and a dance tempo.
const BPM: f64 = 130.0;

/// Seconds each measured capture runs for.
const CAPTURE_SECONDS: f32 = 1.5;

/// Blocks of settling before a capture, so it starts inside steady-state
/// playback rather than on the device's first block after a change.
const SETTLE: Duration = Duration::from_millis(500);

/// The kick's band. The kit's kick sweeps 150 -> 50 Hz.
const KICK_BAND: (f64, f64) = (30.0, 140.0);

/// Tolerance on a measured partial, in Hz. The FFT's bin spacing at these
/// lengths is a couple of Hz and the peaks are parabolically interpolated.
const HZ_TOLERANCE: f64 = 5.0;

/// Equal-tempered pitches this test asserts on.
const A4: f64 = 440.0;
const C4: f64 = 261.63;
const E4: f64 = 329.63;
const G4: f64 = 392.00;
/// C4 with the fixture's +12 semitone state applied.
const C5: f64 = 523.25;

/// Energy in a frequency band, as dBFS.
fn band_dbfs(spectrum: &Spectrum, band: (f64, f64)) -> f32 {
    if spectrum.is_empty() || spectrum.bin_hz <= 0.0 {
        return -200.0;
    }
    let low = (band.0 / spectrum.bin_hz).floor().max(1.0) as usize;
    let high = ((band.1 / spectrum.bin_hz).ceil() as usize).min(spectrum.magnitudes.len() - 1);
    if low > high {
        return -200.0;
    }
    let energy: f64 = spectrum.magnitudes[low..=high]
        .iter()
        .map(|m| f64::from(*m) * f64::from(*m))
        .sum();
    if energy <= 0.0 {
        return -200.0;
    }
    20.0 * energy.sqrt().log10() as f32
}

/// Whether the spectrum has a local maximum within [`HZ_TOLERANCE`] of `hz`.
fn has_partial(spectrum: &Spectrum, hz: f64) -> bool {
    spectrum
        .top_peaks(32)
        .iter()
        .any(|peak| (peak.frequency_hz - hz).abs() <= HZ_TOLERANCE)
}

/// The shell's main thread, played by this test.
///
/// `ClapHost` is `!Send` and lives here; commands are posted from a worker
/// thread exactly as a Tauri command would post them, and served from the loop
/// below exactly as `RunEvent::MainEventsCleared` serves them.
struct Rig {
    state: AppState,
    host: ClapHost,
    bridge: Arc<ClapHostBridge>,
    tracks: Arc<TrackHost>,
    audio: Arc<AudioIoService>,
}

impl Rig {
    fn tick(&mut self) {
        self.host.tick(&self.tracks, &self.audio, &self.bridge);
    }

    /// Post `command` from a worker thread and serve it here, as the shell does.
    fn command(&mut self, command: ClapCommand) -> Result<mydaw_app::ClapAnswer, String> {
        let bridge = Arc::clone(&self.bridge);
        let worker = std::thread::spawn(move || bridge.request(command));
        let deadline = Instant::now() + Duration::from_secs(6);
        while !worker.is_finished() && Instant::now() < deadline {
            self.tick();
            sleep(Duration::from_millis(2));
        }
        worker.join().expect("the requesting thread must not panic")
    }

    /// Put the hosted synth fixture at the head of `track`, through the picker's
    /// own command.
    fn place_synth(&mut self, track: usize, bundle: &Path) {
        self.command(ClapCommand::SetTrackSource {
            track,
            bundle_path: bundle.to_path_buf(),
            plugin_id: SYNTH_ID.to_owned(),
        })
        .unwrap_or_else(|err| panic!("placing the synth on track {track} failed: {err}"));
    }

    /// Hold `key` on `track`, live, the way the keyboard piano does.
    fn hold(&mut self, track: usize, key: u8) {
        self.state
            .set_selected_track(track)
            .expect("the track exists");
        self.state
            .note_on(key, 1.0)
            .expect("the note queue accepts");
    }

    fn release(&mut self, track: usize, key: u8) {
        self.state
            .set_selected_track(track)
            .expect("the track exists");
        self.state.note_off(key).expect("the note queue accepts");
    }

    /// Settle, capture, write the artefacts, print the numbers.
    fn measure(&mut self, label: &str) -> Measured {
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            self.tick();
            sleep(Duration::from_millis(4));
        }
        Measured::take(&self.state, label)
    }
}

/// One measured capture: both channels' spectra, plus the paths on disk.
struct Measured {
    outcome: CaptureOutcome,
    left: Spectrum,
    right: Spectrum,
}

impl Measured {
    fn take(state: &AppState, label: &str) -> Self {
        let outcome = capture_to_disk(state.debug_tap(), CAPTURE_SECONDS, &capture_root())
            .expect("the engine is running, so a capture must arrive");
        let rate = outcome.stats.sample_rate;
        let left = spectrum(&outcome.capture.channel(0), rate);
        let right = spectrum(&outcome.capture.channel(1), rate);
        let measured = Self {
            outcome,
            left,
            right,
        };
        measured.report(label);
        measured
    }

    /// One line an agent can read.
    fn report(&self, label: &str) {
        let stats = &self.outcome.stats;
        eprintln!(
            "[{label}] {} frames @ {} Hz ({:.3} s) | dropped {} | fft {} ({:.2} Hz/bin)",
            stats.frames,
            stats.sample_rate,
            stats.duration_seconds,
            self.outcome.capture.dropped,
            stats.fft_size,
            stats.frequency_resolution_hz,
        );
        let channel = &stats.per_channel[0];
        eprintln!(
            "[{label}]   L peak {:.4} ({:.2} dBFS)  rms {:.5}  kick {:.2} dB",
            channel.peak,
            channel.peak_dbfs,
            channel.rms,
            band_dbfs(&self.left, KICK_BAND),
        );
        let peaks: Vec<String> = self
            .left
            .top_peaks(6)
            .iter()
            .map(|peak| format!("{:.1} Hz", peak.frequency_hz))
            .collect();
        eprintln!("[{label}]   L partials: {}", peaks.join(", "));
        eprintln!("[{label}]   wav  {}", self.outcome.artifacts.wav.display());
        eprintln!("[{label}]   png  {}", self.outcome.artifacts.png.display());
        eprintln!(
            "[{label}]   json {}",
            self.outcome.artifacts.stats_json.display()
        );
    }

    fn has(&self, hz: f64) -> bool {
        has_partial(&self.left, hz) && has_partial(&self.right, hz)
    }

    fn kick(&self) -> f32 {
        band_dbfs(&self.left, KICK_BAND)
    }
}

/// Kick on every beat, and nothing else: a hi-hat is broadband and would smear
/// the partials this test measures.
fn kick_only() -> Pattern {
    let mut pattern = Pattern::with_channels(1, "Pattern 1", &[0, 1, 2, 3, 4], |channel| {
        mydaw_plugin_sequencer::DEFAULT_ROW_KEYS
            .get(channel as usize)
            .copied()
            .unwrap_or(mydaw_plugin_sequencer::DEFAULT_STEP_KEY)
    });
    let kick = pattern.channels[0].steps.as_ref().unwrap().key;
    pattern.channels[0].steps = Some(StepRow::empty(kick).with_steps(&[0, 4, 8, 12]));
    pattern
}

#[test]
#[ignore = "opens the audio device and plays an audible beat and chord"]
fn hosted_clap_instruments_play_through_the_device_and_their_state_is_audible() {
    let staging = std::env::temp_dir().join(format!("mydaw-clap-hw-{}", std::process::id()));
    std::fs::create_dir_all(&staging).expect("scratch dir");
    let Some(bundle) = stage_fixture_bundle(&staging) else {
        eprintln!("{}", missing_fixture_message());
        return;
    };

    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");
    let windows = kernel
        .ctx()
        .services
        .get::<PluginWindowService>()
        .expect("plugin-windows provides its service");

    // The shell's own project: drums on track 0, a synth on track 1.
    let info = match state.start_audio_with_project() {
        Ok(info) => info,
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            let _ = std::fs::remove_dir_all(&staging);
            return;
        }
    };
    eprintln!(
        "device: {} @ {} Hz, {} ch (max block {})",
        info.device_name, info.sample_rate, info.channels, info.max_block
    );

    let bridge = Arc::clone(state.clap_bridge());
    let tracks = Arc::clone(state.tracks());
    let audio = Arc::clone(state.audio());
    let mut host = ClapHost::new(windows);
    host.publish(&tracks, bridge.board());
    bridge.set_served(true);
    let mut rig = Rig {
        state,
        host,
        bridge,
        tracks,
        audio,
    };

    assert_eq!(rig.state.set_bpm(BPM).expect("the tempo is legal"), BPM);
    rig.state
        .set_pattern(kick_only())
        .expect("the beat is valid");
    rig.state.set_playing(true).expect("the transport starts");

    // --- 1. a hosted synth on track 1, alongside the beat on track 0 --------
    rig.place_synth(1, &bundle);
    let placed = rig.state.mixer_state();
    assert_eq!(
        placed.tracks[1].source_id,
        format!("clap:{SYNTH_ID}"),
        "the picker's command must have changed what the mixer reports"
    );
    assert_eq!(
        placed.tracks[1]
            .clap
            .as_ref()
            .map(|slot| slot.plugin_id.as_str()),
        Some(SYNTH_ID)
    );
    assert_eq!(rig.host.instance_count(), 1);

    let beat_only = rig.measure("beat-only");
    assert!(
        beat_only.kick() > -60.0,
        "the beat must be audible on its own: {:.2} dB",
        beat_only.kick()
    );

    rig.hold(1, 69);
    let with_note = rig.measure("beat+hosted-A4");
    assert!(
        with_note.has(A4),
        "the hosted instrument's A4 must be in the same capture as the beat"
    );
    assert!(
        (with_note.kick() - beat_only.kick()).abs() < 6.0,
        "the beat must be unchanged by the plugin: {:.2} -> {:.2} dB",
        beat_only.kick(),
        with_note.kick()
    );
    rig.release(1, 69);

    // --- 2. a chord split across two instances -----------------------------
    // Adding a track with a hosted plugin in the graph is exactly what step 5
    // refused; the rebuild re-attaches the instance that is already placed.
    rig.command(ClapCommand::AddTrack {
        source_id: "poly-synth".to_owned(),
        name: Some("Synth 2".to_owned()),
    })
    .expect("a layout edit with a hosted plugin loaded must succeed");
    assert_eq!(rig.state.mixer_state().tracks.len(), 3);
    rig.place_synth(2, &bundle);
    assert_eq!(rig.host.instance_count(), 2, "two instances, one graph");

    // C4 and E4 on the first instance, G4 on the second: a triad no single
    // instance in this session is playing on its own.
    rig.hold(1, 60);
    rig.hold(1, 64);
    rig.hold(2, 67);
    let chord = rig.measure("chord-split-across-two-instances");
    for (hz, name) in [(C4, "C4"), (E4, "E4"), (G4, "G4")] {
        assert!(
            chord.has(hz),
            "{name} ({hz} Hz) is missing from the split chord"
        );
    }

    // --- 3. state: set, save, change, restore -------------------------------
    let target = ClapTarget::Track { index: 1 };
    let saved = rig
        .command(ClapCommand::SaveState(target))
        .expect("the synth has state")
        .into_state()
        .expect("a blob");
    assert_eq!(
        saved,
        synth::state_blob(0.0),
        "a freshly placed instance saves its default state"
    );

    rig.command(ClapCommand::LoadState {
        target,
        state: synth::state_blob(12.0),
    })
    .expect("the blob loads");
    let detuned = rig.measure("state-detuned-plus-12");
    assert!(
        detuned.has(C5),
        "a +12 semitone state must move track 1's C4 to C5 ({C5} Hz)"
    );
    assert!(
        detuned.has(G4),
        "track 2's G4 must be untouched: the state belongs to one instance"
    );

    rig.command(ClapCommand::LoadState {
        target,
        state: saved.clone(),
    })
    .expect("the saved blob loads back");
    let restored = rig.measure("state-restored");
    assert!(
        restored.has(C4),
        "restoring the saved state must bring C4 ({C4} Hz) back"
    );
    assert!(
        restored.has(E4) && restored.has(G4),
        "and the rest of the chord with it"
    );
    assert_eq!(
        rig.command(ClapCommand::SaveState(target))
            .unwrap()
            .into_state()
            .unwrap(),
        saved,
        "and what it saves is byte-for-byte what was restored"
    );

    // --- teardown, in the shell's order -------------------------------------
    for (track, key) in [(1_usize, 60_u8), (1, 64), (2, 67)] {
        rig.release(track, key);
    }
    rig.state.set_playing(false).ok();
    rig.state.meters().shutdown();
    rig.state.audio().stop();
    rig.host.shutdown();
    assert_eq!(rig.host.instance_count(), 0, "every instance destroyed");
    let _ = std::fs::remove_dir_all(&staging);
}
